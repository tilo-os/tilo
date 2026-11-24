//! An action's id names its resource type and its verb, whether or not another type offers the
//! same verb: a grant, a shortcut or an invocation record naming it keeps matching when the app
//! adds that verb elsewhere. A call chooses between same-named actions by the resource it names
//! first, and the app's own api is sent the verb alone.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::json;
use tilolang::IdsLock;
use tilolang::meta::NamedRootMeta;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{Actor, ResourceRef, Value};
use tilolang_tilo::catalogue;
use tilolang_tilo::host::Host;

const APP: &str = "t.kitchen";

/// A kitchen whose oven can be dismissed, and a counter that stores nothing code reads.
const OVEN_ONLY: &str = r#"
[[resources]]
kind = "kitchen.oven"
label = "Oven"
version = 1
fields = [{ path = "heat", type = "int" }]

[[resources.verbs]]
id = "dismiss"
kind = "outside"
params = []

[[resources]]
kind = "kitchen.timer"
label = "Timer"
version = 1
fields = [{ path = "left", type = "int" }]

[[resources]]
kind = "kitchen.counter"
label = "Counter"
version = 1
fields = []

[[resources.verbs]]
id = "wipe"
kind = "outside"
params = [{ name = "cloth", required = true, type = "text", schema = { type = "string" } }]
"#;

/// The same kitchen, where the timer and the counter offer `dismiss` too.
fn kitchen() -> String {
    OVEN_ONLY
        .replace(
            "fields = [{ path = \"left\", type = \"int\" }]\n",
            "fields = [{ path = \"left\", type = \"int\" }]\n\n[[resources.verbs]]\nid = \"dismiss\"\nkind = \"outside\"\nparams = []\n",
        )
        .replace(
            "label = \"Counter\"\nversion = 1\nfields = []\n",
            "label = \"Counter\"\nversion = 1\nfields = []\n\n[[resources.verbs]]\nid = \"dismiss\"\nkind = \"outside\"\nparams = [{ name = \"reason\", required = true, type = \"text\", schema = { type = \"string\" } }]\n",
        )
}

fn package(text: &str) -> tilolang_runtime::Meta {
    let surfaces = catalogue::parse_surfaces(text).expect("a surface file");
    let mut meta = catalogue::one_action_per_id(catalogue::package_of(APP, "Kitchen", &surfaces))
        .expect("each action once for its type");
    for (name, ty, published) in [
        ("oven", "Oven", "kitchen:oven"),
        ("timer", "Timer", "kitchen:timer"),
    ] {
        meta.named_roots.push(NamedRootMeta {
            name: name.to_owned(),
            ty: format!("{APP}#{ty}"),
            published: Some(published.to_owned()),
            device: false,
        });
    }
    meta
}

/// Each action's id, with the verb the app's api takes it by.
fn ids(meta: &tilolang_runtime::Meta) -> Vec<(&str, &str)> {
    let mut ids: Vec<(&str, &str)> = meta
        .actions
        .iter()
        .map(|action| (action.id.as_str(), action.verb.as_str()))
        .collect();
    ids.sort_unstable();
    ids
}

#[test]
fn a_verbs_id_names_its_type_and_stays_when_another_type_adds_the_verb() {
    let before = package(OVEN_ONLY);
    assert_eq!(
        ids(&before),
        [
            ("t.kitchen#Counter/wipe", "wipe"),
            ("t.kitchen#Oven/dismiss", "dismiss"),
            ("t.kitchen#Oven/share-link", "share-link"),
            ("t.kitchen#Timer/share-link", "share-link"),
        ],
        "a verb only one type offers is named by its type all the same"
    );
    let after = package(&kitchen());
    assert_eq!(
        ids(&after),
        [
            ("t.kitchen#Counter/dismiss", "dismiss"),
            ("t.kitchen#Counter/wipe", "wipe"),
            ("t.kitchen#Oven/dismiss", "dismiss"),
            ("t.kitchen#Oven/share-link", "share-link"),
            ("t.kitchen#Timer/dismiss", "dismiss"),
            ("t.kitchen#Timer/share-link", "share-link"),
        ],
        "the oven's `dismiss` keeps the id a grant or a record already names"
    );
}

/// A view of the oven pressing each kitchen's `dismiss`, and the counter's `wipe`.
const VIEW: &str = r#"app test.kitchen "Kitchen test"

use t.kitchen as k

view oven_view(o: k.Oven) {
  stack {
    text("{o.heat}")
    button("Oven", on: k.dismiss(o)) as oven
    button("Timer", on: k.dismiss(k.timer)) as timer
    button("Counter", on: k.dismiss(reason: "done")) as counter
    button("Wipe", on: k.wipe(cloth: "blue")) as wipe
  }
}
"#;

/// Records what the view invokes of the kitchen.
struct Sent(Rc<RefCell<Vec<Invocation>>>);

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

#[test]
fn a_press_reaches_the_action_of_the_type_it_names_first() {
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .unwrap();
    let meta = package(&kitchen());
    host.runtime().load_foreign(meta.clone()).unwrap();
    let compiled = tilolang::compile_package(VIEW, &IdsLock::new(), std::slice::from_ref(&meta))
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    let package = host.load_compiled(&compiled).unwrap();
    let view = package
        .meta
        .views
        .iter()
        .find(|view| view.name == "oven_view")
        .map(|view| view.id.clone());
    let anchor = |name: &str| {
        package
            .meta
            .views
            .iter()
            .flat_map(|view| &view.anchors)
            .find(|anchor| anchor.name == name)
            .unwrap_or_else(|| panic!("the `{name}` button's anchor"))
            .id
            .clone()
    };
    let oven = ResourceRef::parse_id("kitchen:oven", "t.kitchen#Oven").unwrap();
    let timer = ResourceRef::parse_id("kitchen:timer", "t.kitchen#Timer").unwrap();
    host.runtime()
        .set_reading(&oven, &json!({ "heat": 180 }).to_string())
        .unwrap();
    host.runtime()
        .set_reading(&timer, &json!({ "left": 5 }).to_string())
        .unwrap();
    let shown = host.open_resource(&oven, view.as_deref()).unwrap();
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    for button in ["oven", "timer", "counter", "wipe"] {
        shown
            .press(&anchor(button))
            .unwrap_or_else(|error| panic!("the `{button}` button presses: {error:?}"));
    }
    let sent: Vec<(String, String, Option<String>)> = sent
        .borrow()
        .iter()
        .map(|invocation| match invocation {
            Invocation::Action {
                action,
                name,
                args,
                target,
                ..
            } => (
                action.clone(),
                name.clone(),
                target.as_ref().map(ResourceRef::id_string).or_else(|| {
                    args.iter().find_map(|(_, value)| match value {
                        Value::Ref(r) => Some(r.target.id_string()),
                        _ => None,
                    })
                }),
            ),
            other @ Invocation::Edit { .. } => panic!("an action, not {other:?}"),
        })
        .collect();
    let on = |id: &str| Some(id.to_owned());
    assert_eq!(
        sent,
        [
            (
                "t.kitchen#Oven/dismiss".to_owned(),
                "dismiss".to_owned(),
                on("kitchen:oven")
            ),
            (
                "t.kitchen#Timer/dismiss".to_owned(),
                "dismiss".to_owned(),
                on("kitchen:timer")
            ),
            // No resource named first: the counter's own, which takes none.
            (
                "t.kitchen#Counter/dismiss".to_owned(),
                "dismiss".to_owned(),
                None
            ),
            ("t.kitchen#Counter/wipe".to_owned(), "wipe".to_owned(), None),
        ],
        "each press is its type's own action, sent to the app as the verb"
    );
}

/// Every resource type offers the system's Copy link beside its own verbs: a program calls it on
/// a value as it calls the app's actions, and the host serving the resource mints the link. A
/// kind no program reads (one with no fields and one of no set) keeps its verbs loose.
#[test]
fn every_resource_type_offers_the_systems_copy_link() {
    let meta = package(OVEN_ONLY);
    for ty in ["Oven", "Timer"] {
        let id = format!("t.kitchen#{ty}/share-link");
        let action = meta
            .actions
            .iter()
            .find(|action| action.id == id)
            .unwrap_or_else(|| panic!("the catalogue publishes `{id}`"));
        assert_eq!(action.name, "share_link");
        assert_eq!(action.verb, "share-link");
        assert_eq!(
            action.params.first().map(|param| param.ty.as_str()),
            Some(ty),
            "`{id}` acts on its own {ty}"
        );
        assert_eq!(action.class, tilolang_runtime::meta::EffectClass::Read);
    }
    assert!(
        !meta
            .actions
            .iter()
            .any(|action| action.id == "t.kitchen#Counter/share-link"),
        "a kind that stores nothing code reads is no type"
    );
}

/// The device's notifications and banners each offer `dismiss`, `act` and `reply`: six actions,
/// each named by its type.
#[test]
fn the_devices_banners_and_notifications_keep_their_own_actions() {
    let metas = tilolang_tilo::platform_apps::platform_packages().expect("the platform's packages");
    let device = metas
        .iter()
        .find(|meta| meta.app == tilolang_tilo::platform_apps::DEVICE_APP)
        .expect("the device's package");
    for verb in ["dismiss", "act", "reply"] {
        for ty in ["Banners", "Notifications"] {
            let id = format!("so.tilo.device#{ty}/{verb}");
            let action = device
                .actions
                .iter()
                .find(|action| action.id == id)
                .unwrap_or_else(|| panic!("the device publishes `{id}`"));
            assert_eq!(action.verb, verb);
            assert_eq!(
                action.params.first().map(|param| param.ty.as_str()),
                Some(ty),
                "`{id}` acts on its own {ty}"
            );
        }
    }
}
