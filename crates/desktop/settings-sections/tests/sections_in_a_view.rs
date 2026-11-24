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
use tilolang_tilo::platform_apps::{SECTIONS_APP, on_device, platform_package, platform_roots};
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
    let settings = std::sync::Arc::new(settings_store::workspace::WorkspaceSettings::new());
    settings.attach(settings_store::in_memory::Workspaces::new());
    let _settings = settings_store::scoped_source(Some(settings));
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
            addresses: tilolang_tilo::platform_apps::session_addresses().unwrap(),
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
}
