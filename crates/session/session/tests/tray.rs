//! A running application's status-notifier item, as the session shows it: an
//! application registers over the session bus, the bar names its icon to
//! assistive technology, and choosing one of its menu entries reaches the
//! application.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unused_self,
    clippy::used_underscore_binding,
    clippy::needless_pass_by_value,
    clippy::type_complexity,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: the fakes implement fixed D-Bus signatures, and a \
              failed step is the assertion"
)]

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::ResourceSpec;
use tilo_compositor::OutputConfig;
use tilo_lock::MutexRecover as _;
use tilo_session::desktop_services::{Bus, Buses, DesktopServices};
use tilo_session::{Options, Session};
use tilo_test_env::PrivateBus;
use zbus::zvariant::{Array, Dict, OwnedObjectPath, OwnedValue, StructureBuilder, Value};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const SETTLE: Duration = Duration::from_millis(1);
/// The entry of the fake item's menu this test chooses.
const LOCK_ENTRY: i32 = 1;

/// What the fake application heard from the desktop, oldest first.
type Calls = Arc<Mutex<Vec<String>>>;

/// A status-notifier item as an application publishes one: its name, an
/// icon it sends itself, and the object path of its menu.
struct FakeItem {
    calls: Calls,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl FakeItem {
    #[zbus(property)]
    fn id(&self) -> String {
        "keepassxc".into()
    }

    #[zbus(property)]
    fn title(&self) -> String {
        "KeePassXC".into()
    }

    #[zbus(property)]
    fn status(&self) -> String {
        "Active".into()
    }

    /// One 2×1 icon, so nothing here depends on the host's icon theme.
    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        vec![(2, 1, vec![0xff, 0x10, 0x20, 0x30, 0x80, 0x40, 0x50, 0x60])]
    }

    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from("/MenuBar").unwrap_or_default()
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    fn activate(&self, x: i32, y: i32) {
        self.calls.lock_recover().push(format!("activate {x} {y}"));
    }
}

/// The item's `com.canonical.dbusmenu` menu.
struct FakeMenu {
    calls: Calls,
}

/// One entry of the menu layout.
fn entry(
    id: i32,
    props: &[(&str, Value<'static>)],
    children: Vec<Value<'static>>,
) -> Value<'static> {
    let props: HashMap<String, Value<'static>> = props
        .iter()
        .map(|(key, value)| {
            (
                (*key).to_string(),
                value.try_clone().unwrap_or_else(|_| Value::from("")),
            )
        })
        .collect();
    let structure = StructureBuilder::new()
        .add_field(id)
        .append_field(Value::Dict(Dict::from(props)))
        .append_field(Value::Array(Array::from(children)))
        .build()
        .expect("menu entry structure");
    Value::Structure(structure)
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl FakeMenu {
    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    fn get_layout(
        &self,
        _parent: i32,
        _depth: i32,
        _properties: Vec<String>,
    ) -> (u32, (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>)) {
        let children = vec![
            entry(1, &[("label", Value::from("_Lock database"))], Vec::new()),
            entry(2, &[("label", Value::from("Quit"))], Vec::new()),
        ];
        (
            7,
            (
                0,
                HashMap::new(),
                children
                    .into_iter()
                    .filter_map(|child| OwnedValue::try_from(child).ok())
                    .collect(),
            ),
        )
    }

    fn event(&self, id: i32, event_id: String, _data: OwnedValue, _timestamp: u32) {
        self.calls.lock_recover().push(format!("{event_id} {id}"));
    }
}

/// Step `session` until `asked` answers, for a minute of the machine's time:
/// what it waits for is a bus conversation on another runtime, which a busy
/// builder can take many steps to land.
#[track_caller]
fn until<T>(session: &mut Session, what: &str, mut asked: impl FnMut(&Session) -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        session.step(SETTLE).expect("a frame");
        if let Some(answer) = asked(session) {
            return answer;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the session never showed {what}"
        );
    }
}

/// The resource published for the tray menu entry labelled `label`.
fn entry_resource(resources: &[ResourceSpec], label: &str) -> Option<String> {
    resources.iter().find_map(|resource| {
        if resource.kind == tilo_tray_host::service::ENTRY_KIND && resource.label == label {
            Some(resource.id.as_str().to_owned())
        } else {
            entry_resource(&resource.children, label)
        }
    })
}

/// A headless session whose desktop services serve a session bus of their
/// own, with the services kept alive.
fn session_on(bus: &PrivateBus) -> (Session, Rc<DesktopServices>) {
    let wake = tilo_service::LoopWake::default();
    let (side, link) = tilo_session::portal_link::channel(wake.handle());
    let desktop = Rc::new(
        DesktopServices::start(
            &Buses {
                session: Bus::Address(bus.address().to_owned()),
                system: Bus::None,
            },
            &wake.handle(),
            std::sync::Arc::new(tilo_portal::NoCapture),
            &side,
            None,
        )
        .expect("desktop services"),
    );
    let portal = desktop
        .portal()
        .map(|serving| link.with_captures(serving.input_captures().clone()));
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            services: desktop.services(),
            wake,
            desktop: Some(Rc::clone(&desktop)),
            portal,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    (session, desktop)
}

/// An application registered with the watcher on `bus`: its item and menu
/// served on its own connection, which stays open while the returned
/// connection lives.
fn register_item(desktop: &DesktopServices, bus: &PrivateBus, calls: &Calls) -> zbus::Connection {
    desktop.runtime().block_on(async {
        let app = zbus::connection::Builder::address(bus.address())
            .expect("a session bus address")
            .serve_at(
                "/StatusNotifierItem",
                FakeItem {
                    calls: Arc::clone(calls),
                },
            )
            .expect("serving the item")
            .serve_at(
                "/MenuBar",
                FakeMenu {
                    calls: Arc::clone(calls),
                },
            )
            .expect("serving the menu")
            .build()
            .await
            .expect("the application's connection");
        let watcher = zbus::Proxy::new(
            &app,
            tilo_tray_host::WATCHER_BUS_NAME,
            tilo_tray_host::WATCHER_PATH,
            "org.kde.StatusNotifierWatcher",
        )
        .await
        .expect("the watcher");
        watcher
            .call_method("RegisterStatusNotifierItem", &("/StatusNotifierItem",))
            .await
            .expect("registration answered");
        app
    })
}

/// A running application's item appears on the bar named to assistive
/// technology, and choosing one of its menu entries reaches the application.
#[test]
fn an_indicator_appears_in_the_bar_and_its_menu_item_runs_its_action() {
    let _settled = ui::motion::testing::settled();
    let bus = PrivateBus::start();
    let (mut session, desktop) = session_on(&bus);
    session.step(SETTLE).expect("a frame");
    assert!(
        !session
            .accessible_labels()
            .iter()
            .any(|label| label == "KeePassXC"),
        "no application publishes an item yet"
    );

    let calls: Calls = Arc::default();
    let _app = register_item(&desktop, &bus, &calls);

    // The bar shows the item under the application's own name.
    let named = until(&mut session, "the item on the bar", |session| {
        session
            .accessible_labels()
            .into_iter()
            .find(|label| label == "KeePassXC")
    });
    assert_eq!(named, "KeePassXC", "the bar names the application's item");

    // Its menu is published beside every other action, and choosing an entry
    // reaches the application.
    let lock = until(&mut session, "the item's menu", |session| {
        entry_resource(&session.resources(), "Lock database")
    });
    let mut arguments = serde_json::Map::new();
    arguments.insert("entry".into(), serde_json::json!(LOCK_ENTRY));
    session
        .invoke(&lock, "choose", &arguments)
        .expect("the menu entry is chosen");
    until(
        &mut session,
        "the chosen entry reaching the application",
        |_| {
            calls
                .lock_recover()
                .contains(&format!("clicked {LOCK_ENTRY}"))
                .then_some(())
        },
    );
}
