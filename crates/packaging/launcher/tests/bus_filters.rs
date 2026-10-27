//! The launcher's D-Bus allowlists, enforced by a real `xdg-dbus-proxy`
//! between a sandboxed client and real buses: what a portal-less graphical
//! app may reach on the session bus and on the AT-SPI bus, and that
//! everything else is refused. Calls *into* the app pass the proxy, so the
//! app's own AT-SPI bridge answers other clients read-only: they read its
//! tree, and pressing, typing, setting values and taking focus are refused.
#![allow(
    clippy::unused_self,
    reason = "zbus interface methods are defined on &self"
)]

use std::io::BufRead;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use launcher::dbus_proxy::{ATSPI_FILTER_ARGS, session_filter_args};
use zbus::blocking::Connection;

/// A private bus, as `dbus-run-session` starts one, stopped on drop.
struct Bus {
    daemon: Child,
    address: String,
}

impl Bus {
    fn start(dir: &Path, name: &str) -> Self {
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!("--address=unix:path={}", dir.join(name).display()))
            .stdout(Stdio::piped())
            .spawn()
            .expect("the bus filter tests need dbus-daemon, which the builder image installs");
        let mut address = String::new();
        std::io::BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(!address.trim().is_empty(), "dbus-daemon printed no address");
        Self {
            daemon,
            address: address.trim().to_owned(),
        }
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// `xdg-dbus-proxy` in front of `upstream` with exactly `filter`.
struct Proxy {
    guard: launcher::dbus_proxy::DBusProxyGuard,
}

impl Proxy {
    fn start(upstream: &str, socket: &Path, filter: &[&str]) -> Self {
        let binary = launcher::dbus_proxy::find_proxy_binary()
            .expect("the bus filter tests need xdg-dbus-proxy");
        let guard = launcher::dbus_proxy::spawn_filtered_proxy(&binary, upstream, socket, filter)
            .expect("the production proxy reports its listener ready");
        Self { guard }
    }

    fn connect(&self) -> Connection {
        let address = format!("unix:path={}", self.guard.socket_path().display());
        zbus::blocking::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .expect("the proxy accepts the sandboxed client")
    }
}

fn connect(address: &str) -> zbus::blocking::connection::Builder<'_> {
    zbus::blocking::connection::Builder::address(address).unwrap()
}

/// The accessibility bus launcher's session-bus face.
struct A11yBus;

#[zbus::interface(name = "org.a11y.Bus")]
impl A11yBus {
    fn get_address(&self) -> String {
        "unix:path=/run/user/0/at-spi/bus".to_owned()
    }
    /// Stands for every other method of the launcher: not on the allowlist.
    fn reset(&self) {}
}

struct A11yStatus {
    enabled: bool,
}

#[zbus::interface(name = "org.a11y.Status")]
impl A11yStatus {
    #[zbus(property)]
    fn is_enabled(&self) -> bool {
        self.enabled
    }
    #[zbus(property)]
    fn set_is_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

/// Any other session service, e.g. a secrets store.
struct Other;

#[zbus::interface(name = "org.example.Other")]
impl Other {
    fn ping(&self) -> String {
        "pong".to_owned()
    }
}

/// The AT-SPI registry: the embed socket and the device event controller.
struct Socket;

#[zbus::interface(name = "org.a11y.atspi.Socket")]
impl Socket {
    fn embed(&self) -> String {
        "embedded".to_owned()
    }
}

/// An accessible tree: the registry's own, or another app's.
struct Accessible;

#[zbus::interface(name = "org.a11y.atspi.Accessible")]
impl Accessible {
    fn get_child_count(&self) -> i32 {
        1
    }
}

struct Controller;

#[zbus::interface(name = "org.a11y.atspi.DeviceEventController")]
impl Controller {
    fn get_keystroke_listeners(&self) -> Vec<String> {
        Vec::new()
    }
    /// Stands for injecting input into another client: not on the allowlist.
    fn generate_keyboard_event(&self) {}
}

fn call(
    connection: &Connection,
    destination: &str,
    path: &str,
    interface: &str,
    method: &str,
) -> zbus::Result<zbus::message::Message> {
    connection.call_method(Some(destination), path, Some(interface), method, &())
}

fn property(
    connection: &Connection,
    destination: &str,
    path: &str,
    name: &str,
) -> zbus::Result<zbus::message::Message> {
    connection.call_method(
        Some(destination),
        path,
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &("org.a11y.Status", name),
    )
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one fixture of two buses and their proxies, then every allowed and refused call"
)]
fn a_graphical_app_reaches_only_the_listed_accessibility_calls() {
    let dir = tempfile::tempdir().unwrap();

    // Session bus: the accessibility launcher and an unrelated service.
    let session = Bus::start(dir.path(), "session");
    let _a11y_launcher = connect(&session.address)
        .name("org.a11y.Bus")
        .unwrap()
        .serve_at("/org/a11y/bus", A11yBus)
        .unwrap()
        .serve_at("/org/a11y/bus", A11yStatus { enabled: true })
        .unwrap()
        .build()
        .unwrap();
    let _other = connect(&session.address)
        .name("org.example.Other")
        .unwrap()
        .serve_at("/", Other)
        .unwrap()
        .build()
        .unwrap();

    // AT-SPI bus: the registry, and another app's accessible tree.
    let atspi = Bus::start(dir.path(), "atspi");
    let _registry = connect(&atspi.address)
        .name("org.a11y.atspi.Registry")
        .unwrap()
        .serve_at("/org/a11y/atspi/accessible/root", Socket)
        .unwrap()
        .serve_at("/org/a11y/atspi/accessible/root", Accessible)
        .unwrap()
        .serve_at("/org/a11y/atspi/registry/deviceeventcontroller", Controller)
        .unwrap()
        .build()
        .unwrap();
    let other_app = connect(&atspi.address)
        .serve_at("/org/a11y/atspi/accessible/root", Accessible)
        .unwrap()
        .build()
        .unwrap();
    let other_app_name = other_app.unique_name().unwrap().to_string();

    // A portal-less graphical app: the session filter has only the
    // accessibility status rules.
    let session_proxy = Proxy::start(
        &session.address,
        &dir.path().join("session-proxy"),
        &session_filter_args(false, true),
    );
    let atspi_proxy = Proxy::start(
        &atspi.address,
        &dir.path().join("atspi-proxy"),
        ATSPI_FILTER_ARGS,
    );
    let app_session = session_proxy.connect();
    // The app publishes its accessible tree on its own connection, through
    // the proxy.
    let app_atspi = connect(&format!(
        "unix:path={}",
        atspi_proxy.guard.socket_path().display()
    ))
    .serve_at("/org/a11y/atspi/accessible/root", Accessible)
    .unwrap()
    .build()
    .expect("the proxy accepts the sandboxed client");

    // Session bus: where the AT-SPI bus is, and whether AT is on.
    let reply = call(
        &app_session,
        "org.a11y.Bus",
        "/org/a11y/bus",
        "org.a11y.Bus",
        "GetAddress",
    )
    .expect("GetAddress is allowed");
    let address: String = reply.body().deserialize().unwrap();
    assert_eq!(address, "unix:path=/run/user/0/at-spi/bus");
    property(&app_session, "org.a11y.Bus", "/org/a11y/bus", "IsEnabled")
        .expect("reading IsEnabled is allowed");

    // Nothing else on the session bus.
    assert!(
        call(
            &app_session,
            "org.a11y.Bus",
            "/org/a11y/bus",
            "org.a11y.Bus",
            "Reset"
        )
        .is_err(),
        "only GetAddress of the accessibility launcher is allowed"
    );
    assert!(
        app_session
            .call_method(
                Some("org.a11y.Bus"),
                "/org/a11y/bus",
                Some("org.freedesktop.DBus.Properties"),
                "Set",
                &(
                    "org.a11y.Status",
                    "IsEnabled",
                    zbus::zvariant::Value::from(false)
                ),
            )
            .is_err(),
        "the app must not switch accessibility off"
    );
    assert!(
        call(
            &app_session,
            "org.example.Other",
            "/",
            "org.example.Other",
            "Ping"
        )
        .is_err(),
        "a portal-less app reaches no other session service"
    );

    // AT-SPI bus: embedding its root and the listener queries.
    let reply = call(
        &app_atspi,
        "org.a11y.atspi.Registry",
        "/org/a11y/atspi/accessible/root",
        "org.a11y.atspi.Socket",
        "Embed",
    )
    .expect("Socket.Embed is allowed");
    let embedded: String = reply.body().deserialize().unwrap();
    assert_eq!(embedded, "embedded");
    call(
        &app_atspi,
        "org.a11y.atspi.Registry",
        "/org/a11y/atspi/registry/deviceeventcontroller",
        "org.a11y.atspi.DeviceEventController",
        "GetKeystrokeListeners",
    )
    .expect("GetKeystrokeListeners is allowed");

    // Assistive technology on the AT-SPI bus reads the app's tree: calls
    // into the sandboxed app pass the proxy. The proxy cannot filter calls
    // into the app, so the app's own bridge decides which of them it answers:
    // reads only (`another_client_reads_an_apps_tree_but_cannot_act_on_it`).
    let reader = connect(&atspi.address).build().unwrap();
    let app_name = app_atspi.unique_name().unwrap().to_string();
    let reply = call(
        &reader,
        &app_name,
        "/org/a11y/atspi/accessible/root",
        "org.a11y.atspi.Accessible",
        "GetChildCount",
    )
    .expect("a screen reader reaches the sandboxed app's accessible tree");
    let count: i32 = reply.body().deserialize().unwrap();
    assert_eq!(count, 1);

    // Nothing else on the AT-SPI bus: not the registry's tree, not input
    // injection, and never another app.
    assert!(
        call(
            &app_atspi,
            "org.a11y.atspi.Registry",
            "/org/a11y/atspi/accessible/root",
            "org.a11y.atspi.Accessible",
            "GetChildCount",
        )
        .is_err(),
        "the app must not read the desktop's accessible tree"
    );
    assert!(
        call(
            &app_atspi,
            "org.a11y.atspi.Registry",
            "/org/a11y/atspi/registry/deviceeventcontroller",
            "org.a11y.atspi.DeviceEventController",
            "GenerateKeyboardEvent",
        )
        .is_err(),
        "the app must not inject input"
    );
    assert!(
        call(
            &app_atspi,
            &other_app_name,
            "/org/a11y/atspi/accessible/root",
            "org.a11y.atspi.Accessible",
            "GetChildCount",
        )
        .is_err(),
        "the app must not read another app's accessible tree"
    );
}

/// The AT-SPI registry as an app's adapter meets it: `Socket.Embed` takes the
/// app's root and answers with the registry's own. Records every embedding
/// app's bus name, so the test reaches the app the way a reader would.
#[cfg(target_os = "linux")]
struct RegistrySocket {
    plugs: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

#[cfg(target_os = "linux")]
#[zbus::interface(name = "org.a11y.atspi.Socket")]
impl RegistrySocket {
    fn embed(
        &self,
        plug: (String, zbus::zvariant::OwnedObjectPath),
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> ((String, zbus::zvariant::OwnedObjectPath),) {
        self.plugs.lock().unwrap().push(plug.0);
        let registry = connection
            .unique_name()
            .map(ToString::to_string)
            .unwrap_or_default();
        let root = zbus::zvariant::OwnedObjectPath::try_from(ATSPI_ROOT).unwrap();
        ((registry, root),)
    }
}

#[cfg(target_os = "linux")]
const ATSPI_ROOT: &str = "/org/a11y/atspi/accessible/root";
#[cfg(target_os = "linux")]
const ACCESS_DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";

/// The app's side of the bridge: the tree it publishes, and every request
/// that reached its action handler.
#[cfg(target_os = "linux")]
mod app_tree {
    use std::sync::{Arc, Mutex};

    use accesskit::{
        Action, ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, Node, NodeId,
        Rect, Role, TextPosition, TextSelection, TreeId, TreeInfo, TreeUpdate,
    };

    pub const WINDOW: NodeId = NodeId(0);
    pub const BUTTON: NodeId = NodeId(1);
    pub const FIELD: NodeId = NodeId(2);
    pub const RUN: NodeId = NodeId(3);
    pub const SLIDER: NodeId = NodeId(4);

    /// A window holding a button, a text field with one run and a slider:
    /// one node behind each interface a client could act through.
    pub fn tree() -> TreeUpdate {
        let mut window = Node::new(Role::Window);
        window.set_label("Example");
        window.set_children(vec![BUTTON, FIELD, SLIDER]);
        let mut button = Node::new(Role::Button);
        button.set_label("Confirm");
        button.add_action(Action::Click);
        button.add_action(Action::Focus);
        button.set_bounds(Rect::new(0.0, 0.0, 80.0, 24.0));
        let mut field = Node::new(Role::TextInput);
        field.set_label("Name");
        field.add_action(Action::SetValue);
        field.add_action(Action::Focus);
        field.set_children(vec![RUN]);
        let start = TextPosition {
            node: RUN,
            character_index: 0,
        };
        field.set_text_selection(TextSelection {
            anchor: start,
            focus: start,
        });
        let mut run = Node::new(Role::TextRun);
        run.set_value("Ada");
        run.set_character_lengths([1, 1, 1]);
        let mut slider = Node::new(Role::Slider);
        slider.set_label("Volume");
        slider.set_numeric_value(5.0);
        slider.set_min_numeric_value(0.0);
        slider.set_max_numeric_value(10.0);
        slider.add_action(Action::SetValue);
        TreeUpdate {
            nodes: vec![
                (WINDOW, window),
                (BUTTON, button),
                (FIELD, field),
                (RUN, run),
                (SLIDER, slider),
            ],
            tree: Some(TreeInfo::new(WINDOW)),
            tree_id: TreeId::ROOT,
            focus: WINDOW,
        }
    }

    pub struct App {
        pub requests: Arc<Mutex<Vec<(Action, NodeId)>>>,
    }

    impl ActivationHandler for App {
        fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
            Some(tree())
        }
    }

    impl ActionHandler for App {
        fn do_action(&mut self, request: ActionRequest) {
            self.requests
                .lock()
                .unwrap()
                .push((request.action, request.target_node));
        }
    }

    impl DeactivationHandler for App {
        fn deactivate_accessibility(&mut self) {}
    }
}

/// What a refused call answered: its D-Bus error name.
#[cfg(target_os = "linux")]
fn error_name(result: zbus::Result<zbus::message::Message>) -> String {
    use zbus::DBusError as _;
    match result {
        Ok(_) => "a successful reply".to_owned(),
        Err(zbus::Error::MethodError(name, _, _)) => name.to_string(),
        Err(zbus::Error::FDO(error)) => error.name().to_string(),
        Err(other) => format!("{other:?}"),
    }
}

#[cfg(target_os = "linux")]
fn children(reader: &Connection, app: &str, path: &str) -> Vec<String> {
    call(
        reader,
        app,
        path,
        "org.a11y.atspi.Accessible",
        "GetChildren",
    )
    .ok()
    .and_then(|reply| {
        reply
            .body()
            .deserialize::<Vec<(String, zbus::zvariant::OwnedObjectPath)>>()
            .ok()
    })
    .map(|children| {
        children
            .into_iter()
            .map(|(_, path)| path.to_string())
            .collect()
    })
    .unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn name_of(reader: &Connection, app: &str, path: &str) -> Option<String> {
    let reply = reader
        .call_method(
            Some(app),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.a11y.atspi.Accessible", "Name"),
        )
        .ok()?;
    let value: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
    String::try_from(value).ok()
}

#[cfg(target_os = "linux")]
fn interfaces_of(reader: &Connection, app: &str, path: &str) -> Vec<String> {
    call(
        reader,
        app,
        path,
        "org.a11y.atspi.Accessible",
        "GetInterfaces",
    )
    .expect("every exported node answers GetInterfaces")
    .body()
    .deserialize()
    .expect("GetInterfaces answers a list of interface names")
}

/// A sandboxed app's real accessibility bridge (AccessKit's AT-SPI adapter,
/// as every Tilo app runs it) behind the launcher's AT-SPI proxy: another
/// client on the bus reads the whole tree, but once the launcher's variable
/// tells the app to serve it read-only, pressing, typing, setting a value and
/// taking focus are refused before they reach the app.
#[cfg(target_os = "linux")]
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one fixture of two buses, the app's bridge and a reader, then each read and each refused act"
)]
fn another_client_reads_an_apps_tree_but_cannot_act_on_it() {
    use std::sync::{Arc, Mutex};

    use app_tree::BUTTON;

    let dir = tempfile::tempdir().unwrap();

    // Session bus: the accessibility bus launcher, whose IsEnabled the
    // adapter follows.
    let session = Bus::start(dir.path(), "session");
    let _a11y_launcher = connect(&session.address)
        .name("org.a11y.Bus")
        .unwrap()
        .serve_at("/org/a11y/bus", A11yBus)
        .unwrap()
        .serve_at("/org/a11y/bus", A11yStatus { enabled: false })
        .unwrap()
        .build()
        .unwrap();
    let switch = connect(&session.address).build().unwrap();

    // AT-SPI bus: the registry, the launcher's proxy in front of it, and a
    // reader that is not the app.
    let atspi = Bus::start(dir.path(), "atspi");
    let plugs = Arc::new(Mutex::new(Vec::new()));
    let _registry = connect(&atspi.address)
        .name("org.a11y.atspi.Registry")
        .unwrap()
        .serve_at(
            ATSPI_ROOT,
            RegistrySocket {
                plugs: Arc::clone(&plugs),
            },
        )
        .unwrap()
        .build()
        .unwrap();
    let atspi_proxy = Proxy::start(
        &atspi.address,
        &dir.path().join("atspi-proxy"),
        ATSPI_FILTER_ARGS,
    );
    let reader = connect(&atspi.address).build().unwrap();

    // The app: the adapter finds the AT-SPI bus where the launcher points a
    // sandboxed app's (`AT_SPI_BUS_ADDRESS`, the proxy's socket).
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(
        "AT_SPI_BUS_ADDRESS",
        format!("unix:path={}", atspi_proxy.guard.socket_path().display()),
    )
    .set("DBUS_SESSION_BUS_ADDRESS", &session.address)
    .remove(launcher::dbus_proxy::ATSPI_READ_ONLY_ENV);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let app = || app_tree::App {
        requests: Arc::clone(&requests),
    };
    let _adapter = accesskit_unix::Adapter::new(app(), app(), app());

    // Assistive technology turns on: the adapter follows a change of
    // IsEnabled, so switch it until the app has embedded its tree.
    tilo_test_env::wait_until("the app embeds its accessible tree in the registry", || {
        for enabled in [false, true] {
            switch
                .call_method(
                    Some("org.a11y.Bus"),
                    "/org/a11y/bus",
                    Some("org.freedesktop.DBus.Properties"),
                    "Set",
                    &(
                        "org.a11y.Status",
                        "IsEnabled",
                        zbus::zvariant::Value::from(enabled),
                    ),
                )
                .expect("the test switches assistive technology on its own bus");
        }
        !plugs.lock().unwrap().is_empty()
    });

    // The reader walks the app's tree from its root, through the proxy.
    let (app_name, nodes) = tilo_test_env::wait_for("the reader walks the app's tree", || {
        let app_name = plugs.lock().unwrap().last().cloned()?;
        let window = children(&reader, &app_name, ATSPI_ROOT)
            .into_iter()
            .next()?;
        let nodes: Vec<(String, String)> = children(&reader, &app_name, &window)
            .into_iter()
            .filter_map(|path| Some((name_of(&reader, &app_name, &path)?, path)))
            .collect();
        (nodes.len() == 3).then_some((app_name, nodes))
    });
    let node = |name: &str| -> String {
        let found = nodes
            .iter()
            .find(|(label, _)| label == name)
            .map(|(_, path)| path.clone());
        assert!(found.is_some(), "the reader finds {name:?} in {nodes:?}");
        found.unwrap_or_default()
    };
    let (button, field, slider) = (node("Confirm"), node("Name"), node("Volume"));
    for (path, interface) in [
        (&button, "org.a11y.atspi.Action"),
        (&button, "org.a11y.atspi.Component"),
        (&field, "org.a11y.atspi.EditableText"),
        (&field, "org.a11y.atspi.Text"),
        (&slider, "org.a11y.atspi.Value"),
    ] {
        assert!(
            interfaces_of(&reader, &app_name, path)
                .iter()
                .any(|name| name == interface),
            "{path} offers {interface}, so a refusal below is the read-only tree, \
             not a missing interface"
        );
    }

    // Without the launcher's read-only variable, any client presses the
    // button.
    let reply = reader
        .call_method(
            Some(app_name.as_str()),
            button.as_str(),
            Some("org.a11y.atspi.Action"),
            "DoAction",
            &(0_i32,),
        )
        .expect("without the read-only tree, DoAction reaches the app");
    assert!(reply.body().deserialize::<bool>().unwrap());
    assert_eq!(
        *requests.lock().unwrap(),
        vec![(accesskit::Action::Click, BUTTON)],
        "the press reached the app's action handler: the hole a Tilo app closes"
    );

    // What the launcher sets in a sandboxed app's environment on a release
    // image (`dbus_proxy::atspi_read_only`).
    env.set(launcher::dbus_proxy::ATSPI_READ_ONLY_ENV, "1");

    // Reading still works.
    assert_eq!(
        name_of(&reader, &app_name, &button).as_deref(),
        Some("Confirm")
    );
    let text: String = reader
        .call_method(
            Some(app_name.as_str()),
            field.as_str(),
            Some("org.a11y.atspi.Text"),
            "GetText",
            &(0_i32, -1_i32),
        )
        .expect("a reader reads a field's text")
        .body()
        .deserialize()
        .unwrap();
    assert_eq!(text, "Ada");
    let actions: Vec<(String, String, String)> = reader
        .call_method(
            Some(app_name.as_str()),
            button.as_str(),
            Some("org.a11y.atspi.Action"),
            "GetActions",
            &(),
        )
        .expect("a reader still learns what a control does")
        .body()
        .deserialize()
        .unwrap();
    assert_eq!(actions.len(), 1, "the button still says it can be pressed");

    // Acting is refused, and nothing reaches the app.
    let app_dest = Some(app_name.as_str());
    assert_eq!(
        error_name(reader.call_method(
            app_dest,
            button.as_str(),
            Some("org.a11y.atspi.Action"),
            "DoAction",
            &(0_i32,),
        )),
        ACCESS_DENIED,
        "another client must not press the app's button"
    );
    assert_eq!(
        error_name(reader.call_method(
            app_dest,
            field.as_str(),
            Some("org.a11y.atspi.EditableText"),
            "SetTextContents",
            &("Eve",),
        )),
        ACCESS_DENIED,
        "another client must not type into the app's field"
    );
    assert_eq!(
        error_name(reader.call_method(
            app_dest,
            field.as_str(),
            Some("org.a11y.atspi.EditableText"),
            "InsertText",
            &(0_i32, "Eve", 3_i32),
        )),
        ACCESS_DENIED,
        "another client must not insert text into the app's field"
    );
    assert_eq!(
        error_name(reader.call_method(
            app_dest,
            slider.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Set",
            &(
                "org.a11y.atspi.Value",
                "CurrentValue",
                zbus::zvariant::Value::from(7.0_f64),
            ),
        )),
        ACCESS_DENIED,
        "another client must not set the app's values"
    );
    assert_eq!(
        error_name(reader.call_method(
            app_dest,
            button.as_str(),
            Some("org.a11y.atspi.Component"),
            "GrabFocus",
            &(),
        )),
        ACCESS_DENIED,
        "another client must not move the app's focus"
    );
    assert_eq!(
        error_name(reader.call_method(
            app_dest,
            field.as_str(),
            Some("org.a11y.atspi.Text"),
            "SetCaretOffset",
            &(1_i32,),
        )),
        ACCESS_DENIED,
        "another client must not move the app's caret"
    );
    assert_eq!(
        *requests.lock().unwrap(),
        vec![(accesskit::Action::Click, BUTTON)],
        "no refused call reached the app's action handler"
    );
}
