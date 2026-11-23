//! A tilolang view over the settings sections the session keeps: it reads the person's
//! appearance and this device's power where the session publishes them, its buttons call their
//! setters, the session's settings service runs them, and the view shows the change.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::Value as Json;
use tilo_service::Services;
use tilolang_tilo::host::Host;
use tilolang_tilo::platform_apps::{on_device, platform_packages, platform_roots};
use tilolang_tilo::runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_tilo::runtime::{Actor, ResourceRef, Value, ViewInstance};
use tilolang_tilo::tilolang::{IdsLock, compile_package};

/// This device's key, in lowercase hex, as the session names it.
const DEVICE: &str = "abababababababababababababababababababababababababababababababab";

const VIEW: &str = r#"app test.sections "Sections"

use so.tilo.settings as settings

view looks(a: settings.Appearance) {
  stack {
    text("{a.mode}")
    button("Light", on: settings.set_mode(a, "light")) as light
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

/// Sends what a view invokes of Settings to the session's settings service, at the address the
/// session publishes the resource under, as the shell's places do.
struct Session(Rc<RefCell<Services>>);

impl Invoker for Session {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        let Invocation::Action {
            app, name, target, ..
        } = invocation
        else {
            return Invoked::Failed("a section is changed through its setters".to_owned());
        };
        let target = target.as_ref().expect("a setter names its section");
        let (_, resource) = root(&target.ty);
        // The setter as the settings publish it, not as the view's package names it.
        let name = tilolang_tilo::catalogue::published_verb(app.as_str(), name);
        match self
            .0
            .borrow_mut()
            .invoke(&resource, name, &invocation.api_arguments())
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
    let metas = platform_packages().unwrap();
    for meta in &metas {
        host.runtime().load_foreign(meta.clone()).unwrap();
    }
    let compiled = compile_package(VIEW, &IdsLock::new(), &metas)
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    let package = host.load_compiled(&compiled).unwrap();
    host.runtime().set_invoker(Session(Rc::clone(&services)));
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

    // The person's appearance, at `settings/appearance`, set to light through `set-mode`.
    let (appearance, published) = root("so.tilo.settings#Appearance");
    assert_eq!(published, "settings/appearance");
    publish(&host, &services, &appearance, &published);
    let mut looks = host
        .open_resource(&appearance, Some(&declared("looks")))
        .unwrap();
    assert_ne!(shown(&looks), Value::text("light"), "{:?}", looks.tree());
    looks
        .press(&anchor("light"))
        .expect("the button calls set-mode");
    assert_eq!(
        services.borrow().read(&published).unwrap()["mode"],
        "light",
        "the session's settings hold the change"
    );
    publish(&host, &services, &appearance, &published);
    looks.poll_patches().unwrap();
    assert_eq!(shown(&looks), Value::text("light"), "{:?}", looks.tree());

    // This device's power, at `devices/<device>/settings/power`, locked sooner through
    // `set-lock-minutes`.
    let (power, published) = root("so.tilo.settings#Power");
    assert_eq!(published, format!("devices/{DEVICE}/settings/power"));
    publish(&host, &services, &power, &published);
    let mut lock = host.open_resource(&power, Some(&declared("lock"))).unwrap();
    assert_ne!(shown(&lock), Value::text("3"), "{:?}", lock.tree());
    lock.press(&anchor("sooner"))
        .expect("the button calls set-lock-minutes");
    assert_eq!(
        services.borrow().read(&published).unwrap()["lock_minutes"],
        3,
        "this device's settings hold the change"
    );
    publish(&host, &services, &power, &published);
    lock.poll_patches().unwrap();
    assert_eq!(shown(&lock), Value::text("3"), "{:?}", lock.tree());
}
