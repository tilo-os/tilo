//! A tilolang view over the settings sections the session keeps: it reads the person's
//! appearance and this device's power where the session publishes them, its buttons call their
//! setters and its bound switch sets its field through the field's setter, the session's settings
//! service runs them, and the view shows the change.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::Value as Json;
use tilo_service::Services;
use tilolang_tilo::call::Callees;
use tilolang_tilo::host::Host;
use tilolang_tilo::platform::{SECTIONS_APP, on_device, platform_package, platform_roots};
use tilolang_tilo::runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_tilo::runtime::{Actor, EditOutcome, ResourceRef, Value, ViewInstance};
use tilolang_tilo::tilolang::{IdsLock, compile_package};

/// This device's key, in lowercase hex, as the session names it.
const DEVICE: &str = "abababababababababababababababababababababababababababababababab";

const VIEW: &str = r#"app test.sections "Sections"

use so.tilo.settings.sections as settings

view looks(a: settings.Appearance) {
  stack {
    text("{a.mode}")
    button("Light", on: settings.set_mode(a, "light")) as light
    toggle(bind: a.reduce_motion) as calm
  }
}

view lock(p: settings.Power) {
  stack {
    text("{p.lock_minutes}")
    button("Sooner", on: settings.set_lock_minutes(p, 3)) as sooner
  }
}

view widgets(w: settings.Widgets) {
  stack {
    text("{w.team.enabled}")
    button("No people", on: settings.set_widget_enabled(w, widget: "team", enabled: false)) as off
    button("Music always", on: settings.set_widget_show(w, widget: "music", show: "always")) as always
    button("People at the edge", on: settings.place_widget(w, widget: "team", display: "output:1", x: 1.0, y: 0.5)) as corner
    button("Keep who is here", on: settings.keep_people(w, kept: true, edge: "bottom_left")) as kept
  }
}

view hints(h: settings.ControlHints) {
  stack {
    text("{h.learned.enabled}")
    button("Moved", on: settings.learn_control_hint(h, hint: "move-window")) as learn
    button("Shown", on: settings.note_control_hint_shown(h, hint: "pan-view")) as shown
    button("Not shown", on: settings.withdraw_control_hint_shown(h, hint: "pan-view")) as withdrawn
    button("No hints", on: settings.show_control_hints(h, enabled: false)) as no_hints
    button("Again", on: settings.show_control_hints_again(h)) as again
  }
}
view screens(p: settings.DisplayPerson) {
  stack {
    text("{p.placement.len()}")
    for entry in p.placement {
      text("{entry.device} at {entry.x},{entry.y}")
    }
    button("Place the phone's screen", on: settings.place_display(p, device: "phone", output: "connector:DP-1", x: 3200, y: 0)) as place
  }
}

"#;

/// The resource a view names `ty` by, and where the session publishes it on this device.
fn root(ty: &str) -> (ResourceRef, String) {
    let (published, ty) = platform_roots()
        .into_iter()
        .find(|(_, root)| root == ty)
        .unwrap_or_else(|| panic!("{ty} is one of the platform's resources"));
    let reference = ResourceRef::parse_id(&published, ty).expect("a resource id");
    (reference, on_device(&published, DEVICE))
}

/// Sends what a view invokes of Settings to the session's settings service as the call the
/// platform's API makes of it ([`Callees`]): the setter and its arguments, at the address the
/// session publishes the section under on this device.
struct Session {
    services: Rc<RefCell<Services>>,
    callees: Callees,
}

impl Invoker for Session {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        let call = match self.callees.call(invocation) {
            Ok(call) => call,
            Err(answered) => return Invoked::Failed(format!("{answered:?}")),
        };
        // The section where the session publishes it on this device, by its type, as the shell's
        // places find it.
        let target = match invocation {
            Invocation::Action { target, .. } => {
                target.as_ref().expect("a setter names its section")
            }
            Invocation::Edit { target, .. } => target,
        };
        let (_, resource) = root(&target.ty);
        let name = call
            .get("action")
            .and_then(Json::as_str)
            .expect("a call names its action");
        let arguments = call
            .get("arguments")
            .and_then(Json::as_object)
            .cloned()
            .unwrap_or_default();
        match self
            .services
            .borrow_mut()
            .invoke(&resource, name, &arguments)
        {
            Some(Ok(_)) => Invoked::Done(Value::Unit),
            Some(Err(error)) => Invoked::Failed(error.to_string()),
            None => Invoked::Failed(format!("{resource} has no {name}")),
        }
    }
}

/// What `view` shows as its text.
fn shown(view: &ViewInstance) -> Value {
    fn first(node: &tilolang_tilo::runtime::UiNode) -> Option<Value> {
        if node.kind == "ui.Text" {
            return node.props.get("value").cloned();
        }
        node.children.iter().find_map(first)
    }
    first(view.tree()).expect("the view shows a text")
}

/// Hands `host` the reading the session publishes of `published` for the view's `reference`.
fn publish(host: &Host, services: &RefCell<Services>, reference: &ResourceRef, published: &str) {
    let reading: Json = services
        .borrow()
        .read(published)
        .unwrap_or_else(|| panic!("the session publishes {published}"));
    host.runtime()
        .set_reading(reference, &reading.to_string())
        .expect("the reading is handed in");
}

/// What the session publishes of `published` now, as the view's reader sees it.
fn reading(services: &RefCell<Services>, published: &str) -> Json {
    services
        .borrow()
        .read(published)
        .expect("the session publishes what the view reads")
}

#[test]
fn a_view_sets_the_persons_appearance_and_this_devices_lock_through_their_setters() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    // The settings this process reads and writes, kept in the person's and the device owner's
    // workspaces in memory.
    let _settings = settings_sections::in_memory::scoped();
    ui::framework::read_settings_on(DEVICE);
    let mut service = settings_sections::service::SettingsService::new();
    tilo_service::Service::own_person(&mut service, "ada", &[]);
    let mut services = Services::default();
    services.add(Box::new(service));
    let services = Rc::new(RefCell::new(services));

    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .unwrap();
    let metas = vec![platform_package(SECTIONS_APP).unwrap()];
    for meta in &metas {
        host.runtime().load_foreign(meta.clone()).unwrap();
    }
    let compiled = compile_package(VIEW, &IdsLock::new(), &metas)
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    let package = host.load_compiled(&compiled).unwrap();
    host.runtime().set_invoker(Session {
        services: Rc::clone(&services),
        callees: Callees {
            apps: metas
                .iter()
                .map(|meta| (meta.app.clone(), meta.clone()))
                .collect(),
            published: metas.iter().map(|meta| meta.app.clone()).collect(),
            addresses: tilolang_tilo::platform::session_addresses().unwrap(),
            ..Callees::default()
        },
    });
    let declared = |name: &str| {
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
    appearance_section(&host, &services, &declared, &anchor);
    power_section(&host, &services, &declared, &anchor);
    control_hints_section(&host, &services, &declared, &anchor);
}

/// The person's appearance at `settings/appearance`: a press sets the mode, the switch bound to
/// `reduce_motion` sets its field through the field's setter, and undo takes that step back, the
/// view showing each.
fn appearance_section(
    host: &Host,
    services: &Rc<RefCell<Services>>,
    declared: &impl Fn(&str) -> String,
    anchor: &impl Fn(&str) -> String,
) {
    // The person's appearance, at `settings/appearance`, set to light through `set-mode`.
    let (appearance, published) = root("so.tilo.settings.sections#Appearance");
    assert_eq!(published, "settings/appearance");
    publish(host, services, &appearance, &published);
    let mut looks = host
        .open_resource(&appearance, Some(&declared("looks")))
        .expect("the view opens on the appearance");
    assert_ne!(shown(&looks), Value::text("light"), "{:?}", looks.tree());
    looks
        .press(&anchor("light"))
        .expect("the button calls set-mode");
    assert_eq!(
        reading(services, &published)
            .get("mode")
            .and_then(Json::as_str),
        Some("light"),
        "the session's settings hold the change"
    );
    publish(host, services, &appearance, &published);
    looks
        .poll_patches()
        .expect("the view takes the published change");
    assert_eq!(shown(&looks), Value::text("light"), "{:?}", looks.tree());

    // The switch bound to `reduce_motion` sets it through `set-reduce-motion`: a section
    // publishes no generic field edit.
    assert_eq!(looks.bound_value(&anchor("calm")), Some(Value::Bool(false)));
    assert_eq!(
        looks
            .edit(&anchor("calm"), &Value::Bool(true))
            .expect("the switch's edit is taken"),
        EditOutcome::Applied
    );
    assert_eq!(
        reading(services, &published)
            .get("reduce_motion")
            .and_then(Json::as_bool),
        Some(true),
        "the bound edit reached the field's setter"
    );
    publish(host, services, &appearance, &published);
    looks
        .poll_patches()
        .expect("the view takes the published change");
    assert_eq!(looks.bound_value(&anchor("calm")), Some(Value::Bool(true)));

    // The setter's change is the person's step, as any action's is: undo takes it back, and the
    // switch shows the field as it stands again.
    services
        .borrow_mut()
        .invoke(&published, "undo", &serde_json::Map::new())
        .expect("the undo runs")
        .expect("the undo answers");
    assert_eq!(
        reading(services, &published)
            .get("reduce_motion")
            .and_then(Json::as_bool),
        Some(false),
        "undo reverts the bound edit's setter"
    );
    publish(host, services, &appearance, &published);
    looks
        .poll_patches()
        .expect("the view takes the published change");
    assert_eq!(looks.bound_value(&anchor("calm")), Some(Value::Bool(false)));
}

/// This device's power at `devices/<device>/settings/power`: a press locks it sooner.
fn power_section(
    host: &Host,
    services: &Rc<RefCell<Services>>,
    declared: &impl Fn(&str) -> String,
    anchor: &impl Fn(&str) -> String,
) {
    // This device's power, at `devices/<device>/settings/power`, locked sooner through
    // `set-lock-minutes`.
    let (power, published) = root("so.tilo.settings.sections#Power");
    assert_eq!(published, format!("devices/{DEVICE}/settings/power"));
    publish(host, services, &power, &published);
    let mut lock = host
        .open_resource(&power, Some(&declared("lock")))
        .expect("the view opens on the power section");
    assert_ne!(shown(&lock), Value::text("3"), "{:?}", lock.tree());
    lock.press(&anchor("sooner"))
        .expect("the button calls set-lock-minutes");
    assert_eq!(
        reading(services, &published)
            .get("lock_minutes")
            .and_then(Json::as_u64),
        Some(3),
        "this device's settings hold the change"
    );
    publish(host, services, &power, &published);
    lock.poll_patches()
        .expect("the view takes the published change");
    assert_eq!(shown(&lock), Value::text("3"), "{:?}", lock.tree());

    // This device's widgets: kept off, shown always, and moved to an edge, each through the
    // action the shell and Settings call. The people here are kept when nothing has chosen.
    let (widgets, published) = root("so.tilo.settings.sections#Widgets");
    assert_eq!(published, format!("devices/{DEVICE}/settings/widgets"));
    publish(host, services, &widgets, &published);
    let kept = host
        .open_resource(&widgets, Some(&declared("widgets")))
        .expect("the view opens on the widgets");
    assert_eq!(shown(&kept), Value::text("true"), "{:?}", kept.tree());
    kept.press(&anchor("off"))
        .expect("the button calls set-widget-enabled");
    assert_eq!(
        reading(services, &published)
            .pointer("/team/enabled")
            .and_then(Json::as_bool),
        Some(false),
        "the session's settings no longer keep the people here"
    );
    kept.press(&anchor("always"))
        .expect("the button calls set-widget-show");
    assert_eq!(
        reading(services, &published)
            .pointer("/music/show")
            .and_then(Json::as_str),
        Some("always")
    );
    kept.press(&anchor("corner"))
        .expect("the button calls place-widget");
    let placed = reading(services, &published)
        .pointer("/team/places")
        .cloned();
    assert_eq!(
        placed,
        Some(serde_json::json!([{ "display": "output:1", "x": 1.0, "y": 0.5 }])),
        "the display it was dropped on and the fractions it rests at"
    );

    // Who is here, kept on screen at the bottom-left corner: the pin and its edge, as the shell's
    // own button and a drag of it to another edge keep them.
    kept.press(&anchor("kept"))
        .expect("the button calls keep-people");
    assert_eq!(
        reading(services, &published)
            .pointer("/people_overlay")
            .cloned(),
        Some(serde_json::json!({ "pinned": true, "edge": "bottom_left" })),
        "the roster is kept at the corner the person dropped it in"
    );
    // The top edge is the bar's: an edge the shell has not is refused, and the pin stays where
    // it was.
    let refused = services.borrow_mut().invoke(
        &published,
        "keep-people",
        &serde_json::json!({ "kept": true, "edge": "top" })
            .as_object()
            .cloned()
            .expect("an object"),
    );
    assert!(
        matches!(refused, Some(Err(_))),
        "the top edge is no edge to keep it at"
    );
    assert_eq!(
        reading(services, &published)
            .pointer("/people_overlay/edge")
            .cloned(),
        Some(serde_json::json!("bottom_left"))
    );

    // A widget nothing names is refused, and the refusal says which names there are: a script
    // that misspells one is told so rather than quietly changing nothing.
    let refused = services.borrow_mut().invoke(
        &published,
        "set-widget-enabled",
        &serde_json::json!({ "widget": "weather", "enabled": true })
            .as_object()
            .cloned()
            .expect("an object"),
    );
    assert!(
        matches!(refused, Some(Err(_))),
        "a widget named nothing is not one of the four"
    );
}

/// The person's control hints at `settings/control-hints`: the record the shell's hints write as
/// the person learns, and Settings and the profile card turn on and off, through its actions. Each
/// is the person's step, which undo takes back, and the hints the record knows are the shell's own.
fn control_hints_section(
    host: &Host,
    services: &Rc<RefCell<Services>>,
    declared: &impl Fn(&str) -> String,
    anchor: &impl Fn(&str) -> String,
) {
    let (hints, published) = root("so.tilo.settings.sections#ControlHints");
    assert_eq!(published, "settings/control-hints");
    publish(host, services, &hints, &published);
    let mut view = host
        .open_resource(&hints, Some(&declared("hints")))
        .expect("the view opens on the control hints");
    assert_eq!(
        shown(&view),
        Value::text("true"),
        "hints show until turned off"
    );
    let learned = |services: &Rc<RefCell<Services>>, field: &str| {
        reading(services, &published)
            .pointer(&format!("/learned/{field}"))
            .cloned()
    };

    view.press(&anchor("learn"))
        .expect("the button calls learn-control-hint");
    view.press(&anchor("shown"))
        .expect("the button calls note-control-hint-shown");
    assert_eq!(
        learned(services, "known"),
        Some(serde_json::json!(["move-window"]))
    );
    assert_eq!(
        learned(services, "shown"),
        Some(serde_json::json!({ "pan-view": 1 }))
    );
    view.press(&anchor("withdrawn"))
        .expect("the button calls withdraw-control-hint-shown");
    assert_eq!(
        learned(services, "shown"),
        Some(serde_json::json!({})),
        "a showing that never reached the screen does not count"
    );
    services
        .borrow_mut()
        .invoke(&published, "undo", &serde_json::Map::new())
        .expect("the undo runs")
        .expect("the undo answers");
    assert_eq!(
        learned(services, "shown"),
        Some(serde_json::json!({ "pan-view": 1 })),
        "undo takes the step back"
    );

    view.press(&anchor("no_hints"))
        .expect("the button calls show-control-hints");
    publish(host, services, &hints, &published);
    view.poll_patches()
        .expect("the view takes the published change");
    assert_eq!(shown(&view), Value::text("false"), "{:?}", view.tree());
    assert_eq!(
        settings_store::control_hints::LEARNED.get(),
        settings_store::control_hints::Learned {
            enabled: false,
            known: vec!["move-window".to_owned()],
            shown: std::collections::BTreeMap::from([("pan-view".to_owned(), 1)]),
        },
        "the profile card's switch reads and writes the same record"
    );
    view.press(&anchor("again"))
        .expect("the button calls show-control-hints-again");
    assert_eq!(learned(services, "known"), Some(serde_json::json!([])));
    assert_eq!(learned(services, "shown"), Some(serde_json::json!({})));

    let refused = services.borrow_mut().invoke(
        &published,
        "learn-control-hint",
        &serde_json::json!({ "hint": "fly" })
            .as_object()
            .cloned()
            .expect("an object"),
    );
    assert!(
        matches!(refused, Some(Err(_))),
        "a hint the shell does not have is refused"
    );
}

/// One of the person's devices running this file's views: a host of its own, whose view's
/// invocations the one settings service `services` answers, as each of the person's devices'
/// sessions reaches the person's settings.
fn device(services: &Rc<RefCell<Services>>) -> (Host, tilolang_tilo::tilolang::Compiled) {
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .expect("a host");
    let metas = vec![platform_package(SECTIONS_APP).expect("the sections' package")];
    for meta in &metas {
        host.runtime()
            .load_foreign(meta.clone())
            .expect("the package loads");
    }
    let compiled = compile_package(VIEW, &IdsLock::new(), &metas)
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    host.load_compiled(&compiled).expect("the package loads");
    host.runtime().set_invoker(Session {
        services: Rc::clone(services),
        callees: Callees {
            apps: metas
                .iter()
                .map(|meta| (meta.app.clone(), meta.clone()))
                .collect(),
            published: metas.iter().map(|meta| meta.app.clone()).collect(),
            addresses: tilolang_tilo::platform::session_addresses().expect("its addresses"),
            ..Callees::default()
        },
    });
    (host, compiled)
}

/// The words `view` draws, in the order it draws them.
fn texts(view: &ViewInstance) -> Vec<String> {
    fn walk(node: &tilolang_tilo::runtime::UiNode, out: &mut Vec<String>) {
        if node.kind == "ui.Text"
            && let Some(Value::Text(text)) = node.props.get("value")
        {
            out.push(text.clone());
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(view.tree(), &mut out);
    out
}

/// A screen one of the person's devices placed is one entry of the person's setting, read by
/// every device of theirs: the laptop places the phone's screen, and the phone's own view shows
/// it where it was put.
#[test]
fn a_display_placed_on_one_device_is_where_another_devices_view_shows_it() {
    let _home = tilo_test_env::TestHome::new().expect("a test home");
    let _settings = settings_sections::in_memory::scoped();
    ui::framework::read_settings_on(DEVICE);
    let mut service = settings_sections::service::SettingsService::new();
    tilo_service::Service::own_person(&mut service, "ada", &[]);
    let mut services = Services::default();
    services.add(Box::new(service));
    let services = Rc::new(RefCell::new(services));

    {
        let (laptop, meta) = device(&services);
        let declared = |name: &str| {
            meta.meta
                .views
                .iter()
                .find(|view| view.name == name)
                .expect("the view's declaration")
                .id
                .clone()
        };
        let anchor = |name: &str| {
            meta.meta
                .views
                .iter()
                .flat_map(|view| &view.anchors)
                .find(|anchor| anchor.name == name)
                .expect("the button's anchor")
                .id
                .clone()
        };
        let (screens, published) = root("so.tilo.settings.sections#DisplayPerson");
        assert_eq!(published, "settings/display-person");
        publish(&laptop, &services, &screens, &published);
        let mine = laptop
            .open_resource(&screens, Some(&declared("screens")))
            .expect("the view opens on the person's displays");
        assert_eq!(texts(&mine), ["0"], "{:?}", mine.tree());
        mine.press(&anchor("place"))
            .expect("the button places the phone's screen");

        assert_eq!(
            reading(&services, &published)
                .pointer("/placement")
                .cloned(),
            Some(serde_json::json!([{
                "device": "phone",
                "output": "connector:DP-1",
                "order": 1024,
                "placed": true,
                "x": 3200,
                "y": 0,
            }])),
            "the person's setting keeps one entry for the screen that was placed"
        );
    }

    // The person's other device reads the same setting and shows the screen where it was put.
    let (phone, meta) = device(&services);
    let declared = |name: &str| {
        meta.meta
            .views
            .iter()
            .find(|view| view.name == name)
            .expect("the view's declaration")
            .id
            .clone()
    };
    let (screens, published) = root("so.tilo.settings.sections#DisplayPerson");
    publish(&phone, &services, &screens, &published);
    let theirs = phone
        .open_resource(&screens, Some(&declared("screens")))
        .unwrap_or_else(|error| panic!("the view opens on the person's displays: {error:?}"));
    assert_eq!(
        texts(&theirs),
        ["1", "phone at 3200,0"],
        "{:?}",
        theirs.tree()
    );
}
