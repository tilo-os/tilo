//! A view over a resource of an app this host does not run: it reads the reading the app
//! publishes, runs again when the reading moves, and its buttons invoke that app's verbs as
//! child invocations of the view's own code.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::json;
use tilolang::IdsLock;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{
    Actor, AmStore, ModuleHash, Patch, ResourceRef, Runtime, SimpleAuthority, Value, ViewInstance,
};
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
  stack {
    text("{p.level}")
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
fn workspace(level: f64) -> (Host, ViewInstance, String, ModuleHash) {
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .unwrap();
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

tilo_test_env::scenario! {
    fn a_view_over_a_published_reading_shows_it_and_invokes_through_the_host;

    phase a_view_shows_the_reading_the_app_publishes;
    {
        let (_host, view, _up, _module) = workspace(0.25);
        assert_eq!(shown(&view), Value::text("0.25"));
    }

    phase a_buttons_invocation_carries_the_views_author_and_the_player;
    {
        let (host, view, up, module) = workspace(0.25);
        let sent = Rc::new(RefCell::new(Vec::new()));
        host.runtime().set_invoker(Sent(Rc::clone(&sent)));
        view.press(&up).expect("the button invokes the app's verb");
        let sent = sent.borrow();
        let [
            Invocation::Action {
                app,
                action,
                name,
                args,
                author,
                target,
                ..
            },
        ] = sent.as_slice()
        else {
            panic!("one invocation of the app's verb: {sent:?}");
        };
        assert_eq!(
            (app.as_str(), action.as_str(), name.as_str()),
            (APP, "sound#set-level", "set-level"),
            "the verb, named here by its app, goes to the app by the name it publishes"
        );
        assert_eq!(target.as_ref(), Some(&player()));
        assert_eq!(author.as_ref(), Some(&module), "the view's own code asked");
        assert_eq!(args.len(), 2, "{args:?}");
        assert_eq!(args[0].1, Value::resource(&player()), "the player, first");
        assert_eq!(args[1], ("level".to_owned(), Value::Float(1.0)));
    }

    phase a_host_that_sends_no_invocation_on_says_so;
    {
        let (_host, view, up, _module) = workspace(0.25);
        let why = view.press(&up).expect_err("nowhere to send it");
        assert!(
            format!("{why}").contains("an app this host does not run"),
            "{why}"
        );
    }
}

/// Two apps that each publish a verb of the same name: an alarm and a timer, both `cancel`.
const ALARM: &str = r#"
[[resources]]
kind = "alarm.alarm"
label = "Alarm"
version = 1
fields = [{ path = "ringing", type = "bool" }]

[[resources.verbs]]
id = "cancel"
label = "Cancel"
kind = "undoable"
"#;

const TIMER: &str = r#"
[[resources]]
kind = "timer.timer"
label = "Timer"
version = 1
fields = [{ path = "left", type = "float" }]

[[resources.verbs]]
id = "cancel"
label = "Cancel"
kind = "undoable"
"#;

/// A view over each app's resource, each with a button for that app's `cancel`.
const BOTH: &str = r#"app test.kitchen "Kitchen"

use alarm as alarm
use timer as timer

view alarm_view(a: alarm.Alarm) {
  button("Stop the alarm", on: alarm.cancel(a)) as stop_alarm
}

view timer_view(t: timer.Timer) {
  button("Stop the timer", on: timer.cancel(t)) as stop_timer
}
"#;

#[test]
fn two_apps_publishing_the_same_verb_both_load_and_each_call_reaches_its_own() {
    let package = |app: &str, title: &str, text: &str| {
        let surfaces = catalogue::parse_surfaces(text).expect("a surface file");
        catalogue::package_of(app, title, &surfaces)
    };
    let alarm = package("alarm", "Alarm", ALARM);
    let timer = package("timer", "Timer", TIMER);
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .unwrap();
    host.runtime()
        .load_foreign(alarm.clone())
        .expect("the alarm's package loads");
    host.runtime()
        .load_foreign(timer.clone())
        .expect("the timer's package loads beside it, its `cancel` its own");
    let compiled = tilolang::compile_package(BOTH, &IdsLock::new(), &[alarm, timer])
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    let package = host.load_compiled(&compiled).unwrap();
    let id_of = |name: &str| {
        package
            .meta
            .views
            .iter()
            .find(|view| view.name == name)
            .expect("the view's declaration")
            .id
            .clone()
    };
    let anchor = |name: &str| {
        package
            .meta
            .views
            .iter()
            .flat_map(|view| &view.anchors)
            .find(|anchor| anchor.name == name)
            .expect("the button's anchor")
            .id
            .clone()
    };
    let alarm_ref = ResourceRef::parse_id("alarm-1", "alarm#Alarm").unwrap();
    let timer_ref = ResourceRef::parse_id("timer-1", "timer#Timer").unwrap();
    host.runtime()
        .set_reading(&alarm_ref, &json!({ "ringing": true }).to_string())
        .unwrap();
    host.runtime()
        .set_reading(&timer_ref, &json!({ "left": 30.0 }).to_string())
        .unwrap();
    let alarm_view = host
        .open_resource(&alarm_ref, Some(&id_of("alarm_view")))
        .unwrap();
    let timer_view = host
        .open_resource(&timer_ref, Some(&id_of("timer_view")))
        .unwrap();
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    alarm_view
        .press(&anchor("stop_alarm"))
        .expect("the alarm's cancel");
    timer_view
        .press(&anchor("stop_timer"))
        .expect("the timer's cancel");
    let reached: Vec<(String, String, Option<ResourceRef>)> = sent
        .borrow()
        .iter()
        .map(|invocation| match invocation {
            Invocation::Action {
                app, name, target, ..
            } => (app.as_str().to_owned(), name.clone(), target.clone()),
            other @ Invocation::Edit { .. } => panic!("an action of an app: {other:?}"),
        })
        .collect();
    assert_eq!(
        reached,
        vec![
            ("alarm".to_owned(), "cancel".to_owned(), Some(alarm_ref)),
            ("timer".to_owned(), "cancel".to_owned(), Some(timer_ref)),
        ],
        "each press goes to its own app, by the verb that app publishes"
    );
}

#[test]
fn a_new_reading_runs_the_view_again() {
    let (host, mut view, _up, _module) = workspace(0.25);
    host.runtime()
        .set_reading(&player(), &json!({ "level": 0.75 }).to_string())
        .unwrap();
    let patches = view.poll_patches().unwrap();
    assert!(
        matches!(
            patches.as_slice(),
            [Patch::SetProp {
                value: Some(Value::Text(level)),
                ..
            }] if level == "0.75"
        ),
        "{patches:?}"
    );
    assert_eq!(shown(&view), Value::text("0.75"));
}

#[test]
fn the_same_reading_twice_changes_nothing() {
    let (host, mut view, _up, _module) = workspace(0.25);
    let again = host
        .runtime()
        .set_reading(&player(), &json!({ "level": 0.25 }).to_string())
        .unwrap();
    assert!(!again, "a reading that did not change moves nothing");
    assert_eq!(view.poll_patches().unwrap(), Vec::new());
}

/// A scenario stands a reading in for the app that publishes it: a view over that app's resource
/// reads what the scenario staged, documents inside it included.
#[test]
fn a_scenario_stages_another_apps_reading() {
    const STAGED: &str = r#"app test.staged "Staged"

use so.tilo.device as device

view centre(c: device.Notifications) {
  text("{c.notices.count(n -> not n.seen)} {c.notices.first()?.title ?? ""}")
}

scenario "one waiting" {
  stage(
    device.notifications,
    device.Notifications(
      notices: [
        device.Notice(number: 1, title: "Tea is ready", body: "", from: "", kind: "info", seen: false),
      ],
    ),
  )
}
"#;
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .unwrap();
    let metas = tilolang_tilo::platform_apps::platform_packages().unwrap();
    for meta in &metas {
        host.runtime().load_foreign(meta.clone()).unwrap();
    }
    let compiled = tilolang::compile_package(STAGED, &IdsLock::new(), &metas)
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    let package = host.load_compiled(&compiled).unwrap();
    let centre = tilolang_runtime::ResourceRef::parse_id(
        "session:notifications",
        "so.tilo.device#Notifications",
    )
    .unwrap();
    host.runtime().set_reading(&centre, "{}").unwrap();
    let scenario = package.meta.scenarios[0].action.clone();
    host.runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (scenario.as_str(), vec![]),
        )
        .unwrap_or_else(|why| panic!("the scenario runs: {why}"));
    let view = package
        .meta
        .views
        .iter()
        .find(|view| view.name == "centre")
        .unwrap()
        .id
        .clone();
    let open = host.open_resource(&centre, Some(&view)).unwrap();
    assert_eq!(
        shown(&open),
        Value::text("1 Tea is ready"),
        "{:?}",
        open.tree()
    );
}

// A view reads the agent model choice the session publishes as
// `settings.agent_models`: its root is a named root of Settings' package, its
// fields the reading's, and its `use-model` the verb the settings service
// answers.
tilo_test_env::scenario! {
    fn a_view_reads_the_agent_model_choice_as_a_settings_root;

    phase the_view_compiles_against_the_settings_package;
    {
        // `settings.agent_models` reads as a named root of Settings' package,
        // as `settings.appearance` does, and its fields are the reading's.
        const VIEW: &str = r#"app test.models "Models"

use so.tilo.settings as settings

view switcher() {
  let m = settings.agent_models
  ui.Text("{m.in_use.model_label}")
}
"#;
        let metas = tilolang_tilo::platform_apps::platform_packages().unwrap();
        tilolang::compile_package(VIEW, &IdsLock::new(), &metas)
            .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    }

    phase the_view_reads_the_root_and_its_verb;
    {
        // The same choice, its `use_model` taking the resource and the
        // provider and model the person picks: the verb is published on the
        // resource, so the view passes it.
        const VIEW: &str = r#"app test.models "Models"

use so.tilo.settings as settings

view switcher(m: settings.AgentModels) {
  stack {
    text("{m.in_use.model_label}")
    button("Use it", on: settings.use_model(m, m.in_use.provider, m.in_use.model)) as pick
  }
}
"#;
        let metas = tilolang_tilo::platform_apps::platform_packages().unwrap();
        tilolang::compile_package(VIEW, &IdsLock::new(), &metas)
            .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    }

    phase the_view_shows_the_model_agents_answer_with;
    {
        const VIEW: &str = r#"app test.models "Models"

use so.tilo.settings as settings

view switcher(m: settings.AgentModels) {
  ui.Text("{m.in_use.model_label}")
}
"#;
        let host = Host::new(
            Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            || None,

    )
        .unwrap();
        let metas = tilolang_tilo::platform_apps::platform_packages().unwrap();
        for meta in &metas {
            host.runtime().load_foreign(meta.clone()).unwrap();
        }
        let compiled = tilolang::compile_package(VIEW, &IdsLock::new(), &metas)
            .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
        let package = host.load_compiled(&compiled).unwrap();
        let reading = tilolang_runtime::ResourceRef::parse_id(
            "settings:settings.agent-models",
            "so.tilo.settings#AgentModels",
        )
        .unwrap();
        host.runtime()
            .set_reading(
                &reading,
                &json!({
                    "providers": [],
                    "in_use": { "provider": "local", "model": "small", "model_label": "Small model" },
                })
                .to_string(),
            )
            .unwrap();
        let view = package
            .meta
            .views
            .iter()
            .find(|view| view.name == "switcher")
            .expect("the view's declaration")
            .id
            .clone();
        let open = host.open_resource(&reading, Some(&view)).unwrap();
        assert_eq!(shown(&open), Value::text("Small model"), "{:?}", open.tree());
    }
}

/// The surface of an app that names a type of its own after each of `names` and keeps one of
/// each, under the name in lower case. An app's author names their types without knowing which
/// names the language holds a type of its own under.
fn named_as_the_language(names: &[&str]) -> String {
    let fields = names
        .iter()
        .map(|name| {
            let path = name.to_lowercase();
            format!("  {{ path = \"{path}\", type = {{ named = \"{name}\" }} }},\n")
        })
        .collect::<Vec<_>>()
        .concat();
    let types = names
        .iter()
        .map(|name| {
            format!(
                "\n[[resources.types]]\nname = \"{name}\"\nfields = [{{ name = \"said\", type = \"text\" }}]\n"
            )
        })
        .collect::<Vec<_>>()
        .concat();
    format!(
        "[[resources]]\nkind = \"post.tray\"\nlabel = \"Tray\"\nversion = 1\nfields = [\n{fields}]\n{types}"
    )
}

#[test]
fn a_type_an_app_names_as_the_language_names_one_of_its_own_stays_the_apps_in_a_reading() {
    let names = tilolang::types::LANGUAGE_TYPE_NAMES;
    let surfaces =
        catalogue::parse_surfaces(&named_as_the_language(names)).expect("a surface file");
    let meta = catalogue::package_of("post", "Post", &surfaces);
    let runtime = Runtime::new(AmStore::new(), SimpleAuthority::new()).expect("a runtime starts");
    runtime.load_foreign(meta).expect("the app's package loads");
    let tray = ResourceRef::root("post.tray".to_owned(), "post#Tray".to_owned());
    let reading: serde_json::Map<String, serde_json::Value> = names
        .iter()
        .map(|name| (name.to_lowercase(), json!({ "said": name })))
        .collect();
    runtime
        .set_reading(&tray, &serde_json::Value::Object(reading).to_string())
        .expect("the reading is taken");
    for name in names {
        let held = match runtime.read(&tray, &name.to_lowercase()) {
            Ok(Value::Ref(held)) => held,
            other => panic!("the app's `{name}` reads as the app's own type, not as {other:?}"),
        };
        let said = runtime
            .read(&held.target, "said")
            .unwrap_or_else(|why| panic!("what the app's `{name}` holds: {why}"));
        assert_eq!(said, Value::text(*name), "what the app's `{name}` holds");
    }
}

/// The surface of an app whose kind is named as the language names a type of its own: the
/// kind `post.view` is the app's `View`, and its verb takes it.
const KIND_NAMED_AS_THE_LANGUAGE: &str = r#"
[[resources]]
kind = "post.view"
label = "Post"
version = 1
fields = [{ path = "title", type = "text" }]

[[resources.verbs]]
id = "show-all"
label = "Show all"
kind = "undoable"
params = []
"#;

/// A view over that kind, whose button calls the kind's verb on it.
const CALLER: &str = r#"app test.caller "Caller"

use post as post

view shown(v: post.View) {
  stack {
    text("{v.title}")
    button("All", on: post.show_all(v)) as all
  }
}
"#;

#[test]
fn a_kind_an_app_names_as_the_language_names_a_type_is_the_apps_where_its_verb_takes_it() {
    let surfaces = catalogue::parse_surfaces(KIND_NAMED_AS_THE_LANGUAGE).expect("a surface file");
    let meta = catalogue::package_of("post", "Post", &surfaces);
    tilolang::compile_package(CALLER, &IdsLock::new(), std::slice::from_ref(&meta)).unwrap_or_else(
        |diagnostics| panic!("a view calls the verb on the app's own kind: {diagnostics:#?}"),
    );
}

// The workspace's contact directory is a root of Contacts' package a view
// reads as `contacts.directory`: the reading the session publishes
// (`contacts:tilo.contacts.identities`), and its `confirm-endpoint` the verb
// Contacts already publishes, so the account-association panel calls it as it
// is.
tilo_test_env::scenario! {
    fn a_view_reads_the_workspace_directory_and_confirms_an_endpoint;

    phase the_view_compiles_against_the_contacts_package;
    {
        let metas = tilolang_tilo::platform_apps::platform_packages().unwrap();
        tilolang::compile_package(DIRECTORY_VIEW, &IdsLock::new(), &metas)
            .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    }

    phase the_directory_is_a_platform_root_the_session_publishes;
    {
        let roots = tilolang_tilo::platform_apps::platform_roots();
        assert!(
            roots.iter().any(|(published, ty)| {
                *published == "contacts:tilo.contacts.identities"
                    && ty == "so.tilo.contacts#Identities"
            }),
            "{roots:?}"
        );
    }

    phase the_view_shows_one_person_of_the_directory;
    {
        let (host, view, _confirm) = directory_host(DIRECTORY_VIEW);
        set_directory(&host, "Ada");
        let open = host.open_resource(&directory(), Some(&view)).unwrap();
        assert_eq!(shown(&open), Value::text("Ada"), "{:?}", open.tree());
    }

    phase the_buttons_invocation_is_the_directories_own_confirm_endpoint;
    {
        let (host, view, confirm) = directory_host(DIRECTORY_VIEW);
        set_directory(&host, "Ada");
        let open = host.open_resource(&directory(), Some(&view)).unwrap();
        let sent = Rc::new(RefCell::new(Vec::new()));
        host.runtime().set_invoker(Sent(Rc::clone(&sent)));
        open.press(&confirm)
            .expect("the button invokes the directory's verb");
        let sent = sent.borrow();
        let [
            Invocation::Action {
                app,
                action,
                name,
                args,
                target,
                ..
            },
        ] = sent.as_slice()
        else {
            panic!("one invocation of the directory's verb: {sent:?}");
        };
        assert_eq!(
            (app.as_str(), action.as_str(), name.as_str()),
            (
                "so.tilo.contacts",
                "so.tilo.contacts#confirm-endpoint",
                "confirm-endpoint"
            ),
            "Contacts' verb, named here by its app, reaches Contacts as the verb it publishes"
        );
        assert_eq!(target.as_ref(), Some(&directory()));
        assert_eq!(args[0], ("identities".to_owned(), Value::resource(&directory())));
        assert_eq!(args[1], ("person".to_owned(), Value::text("ada")));
        assert_eq!(args[2], ("endpoint".to_owned(), Value::text("endpoint")));
        assert_eq!(args[3], ("service".to_owned(), Value::text("signal")));
        assert_eq!(args[4], ("recipient".to_owned(), Value::text("+12025550123")));
    }
}

/// A view over the workspace's directory: it reads one person's label, and its
/// button confirms an endpoint of that person.
const DIRECTORY_VIEW: &str = r#"app test.people "People"

use so.tilo.contacts as contacts

view directory(d: contacts.Identities) {
  stack {
    text("{d.people.first()?.label ?? ""}")
    button("Confirm", on: contacts.confirm_endpoint(d, "ada", "endpoint", "signal", "+12025550123")) as confirm
  }
}
"#;

/// The workspace's directory, as a view names it: its published id and its type.
fn directory() -> ResourceRef {
    ResourceRef::parse_id(
        "contacts:tilo.contacts.identities",
        "so.tilo.contacts#Identities",
    )
    .expect("a resource id")
}

/// What the session publishes of the directory: one person, `label`.
fn set_directory(host: &Host, label: &str) {
    host.runtime()
        .set_reading(
            &directory(),
            &json!({
                "people": [{
                    "label": label,
                    "removed": false,
                    "phones": ["+12025550123"],
                    "emails": ["ada@example.test"],
                    "photo": "",
                    "endpoints": [{
                        "service": "signal",
                        "recipient": "+12025550123",
                        "scope": "",
                        "revision": "",
                        "removed": false,
                    }],
                }],
            })
            .to_string(),
        )
        .unwrap();
}

/// A host that runs `DIRECTORY_VIEW` over the Contacts package, with the
/// view's id and its `confirm` anchor.
fn directory_host(view: &str) -> (Host, String, String) {
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .unwrap();
    let metas = tilolang_tilo::platform_apps::platform_packages().unwrap();
    let contacts = metas
        .iter()
        .find(|meta| meta.app == "so.tilo.contacts")
        .expect("the Contacts package");
    host.runtime().load_foreign(contacts.clone()).unwrap();
    let compiled = tilolang::compile_package(view, &IdsLock::new(), &metas)
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    let package = host.load_compiled(&compiled).unwrap();
    let view = package
        .meta
        .views
        .iter()
        .find(|view| view.name == "directory")
        .expect("the view's declaration")
        .id
        .clone();
    let confirm = package
        .meta
        .views
        .iter()
        .flat_map(|view| &view.anchors)
        .find(|anchor| anchor.name == "confirm")
        .expect("the button's anchor")
        .id
        .clone();
    (host, view, confirm)
}
