//! The launcher's D-Bus allowlists, enforced by a real `xdg-dbus-proxy`
//! between a sandboxed client and real buses: what a portal-less graphical
//! app may reach on the session bus and on the AT-SPI bus, and that
//! everything else is refused.
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
    // into the sandboxed app pass the proxy.
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
