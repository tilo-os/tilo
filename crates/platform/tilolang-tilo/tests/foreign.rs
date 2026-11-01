//! A view over a resource of an app this host does not run: it reads the reading the app
//! publishes, runs again when the reading moves, and its buttons invoke that app's verbs as
//! child invocations of the view's own code.

use std::cell::RefCell;
use std::rc::Rc;

use automerge::AutoCommit;
use serde_json::json;
use tilolang::IdsLock;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{ModuleHash, Patch, Principal, Value, ViewInstance};
use tilolang_tilo::catalogue;
use tilolang_tilo::host::Host;

/// The surface of the app this host does not run: its player has a level, and a verb that sets
/// it.
const SERVICE: &str = r#"
[[resources]]
kind = "sound.player"
label = "Player"
version = 1
fields = [{ path = "level", type = "float" }]

[[resources.verbs]]
id = "set-level"
label = "Set level"
kind = "undoable"
params = [{ name = "level", required = true, type = "float", schema = { type = "number" } }]
"#;

/// A view over another app's player: what it reads is the app's reading, what its button
/// invokes is the app's verb.
const VIEW: &str = r#"app test.volume "Volume"

use sound as sound

view player(p: sound.Player) {
  column {
    text(p.level)
    button("Up", on: sound.set_level(p, 1.0)) as up
  }
}
"#;

const APP: &str = "sound";
const TYPE: &str = "sound#Player";

/// The app's package, as this host knows it: its surface read as a package.
fn service() -> tilolang_runtime::Meta {
    let surfaces = catalogue::parse_surfaces(SERVICE).expect("a surface file");
    catalogue::package_of(APP, "Sound", &surfaces)
}

/// Records what the view invokes of the app.
struct Sent(Rc<RefCell<Vec<Invocation>>>);

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

/// A host that runs the view and knows the app only by its package, with the app's player
/// reading `level`.
fn room(level: f64) -> (Host, ViewInstance, String, ModuleHash) {
    let doc = AutoCommit::new();
    let host = Host::new(&doc, Principal::new("ada"), || None).unwrap();
    let meta = service();
    host.runtime().load_foreign(meta.clone()).unwrap();
    let compiled = tilolang::compile_package(VIEW, &IdsLock::new(), std::slice::from_ref(&meta))
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    let module = tilolang_runtime::Runtime::module_hash(&compiled.wasm);
    let package = host.load_compiled(&compiled).unwrap();
    let view = package
        .meta
        .views
        .iter()
        .find(|view| view.name == "player")
        .expect("the view's declaration")
        .id
        .clone();
    let up = package
        .meta
        .views
        .iter()
        .flat_map(|view| &view.anchors)
        .find(|anchor| anchor.name == "up")
        .expect("the button's anchor")
        .id
        .clone();
    host.runtime()
        .set_reading(&player(), &json!({ "level": level }).to_string())
        .unwrap();
    let open = host.open_resource(&player(), Some(&view)).unwrap();
    (host, open, up, module)
}

/// The app's player, as a view of ours names it: its app and type, by the id it is published
/// under.
fn player() -> tilolang_runtime::ResourceRef {
    tilolang_runtime::ResourceRef::parse_id("player-1", TYPE).expect("a resource id")
}

/// What the view shows as its text.
fn shown(view: &ViewInstance) -> Value {
    fn first(node: &tilolang_runtime::UiNode) -> Option<Value> {
        if node.kind == "ui.Text" {
            return node.props.get("value").cloned();
        }
        node.children.iter().find_map(first)
    }
    first(view.tree()).expect("the view shows its level")
}

#[test]
fn a_view_shows_the_reading_the_app_publishes() {
    let (_host, view, _up, _module) = room(0.25);
    assert_eq!(shown(&view), Value::Float(0.25));
}

#[test]
fn a_new_reading_runs_the_view_again() {
    let (host, mut view, _up, _module) = room(0.25);
    host.runtime()
        .set_reading(&player(), &json!({ "level": 0.75 }).to_string())
        .unwrap();
    let patches = view.poll_patches().unwrap();
    assert!(
        matches!(
            patches.as_slice(),
            [Patch::SetProp {
                value: Some(Value::Float(level)),
                ..
            }] if (*level - 0.75).abs() < f64::EPSILON
        ),
        "{patches:?}"
    );
    assert_eq!(shown(&view), Value::Float(0.75));
}

#[test]
fn the_same_reading_twice_changes_nothing() {
    let (host, mut view, _up, _module) = room(0.25);
    let again = host
        .runtime()
        .set_reading(&player(), &json!({ "level": 0.25 }).to_string())
        .unwrap();
    assert!(!again, "a reading that did not change moves nothing");
    assert_eq!(view.poll_patches().unwrap(), Vec::new());
}

#[test]
fn a_buttons_invocation_carries_the_views_author_and_the_player() {
    let (host, view, up, module) = room(0.25);
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    view.press(&up).expect("the button invokes the app's verb");
    let sent = sent.borrow();
    let [
        Invocation::Action {
            app,
            action,
            args,
            author,
            subject,
            ..
        },
    ] = sent.as_slice()
    else {
        panic!("one invocation of the app's verb: {sent:?}");
    };
    assert_eq!((app.as_str(), action.as_str()), (APP, "set-level"));
    assert_eq!(subject.as_ref(), Some(&player()));
    assert_eq!(author.as_ref(), Some(&module), "the view's own code asked");
    assert_eq!(args.len(), 2, "{args:?}");
    assert_eq!(args[0].1, Value::resource(&player()), "the player, first");
    assert_eq!(args[1], ("level".to_owned(), Value::Float(1.0)));
}

#[test]
fn a_host_that_sends_no_invocation_on_says_so() {
    let (_host, view, up, _module) = room(0.25);
    let why = view.press(&up).expect_err("nowhere to send it");
    assert!(
        format!("{why}").contains("an app this host does not run"),
        "{why}"
    );
}
