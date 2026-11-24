//! The launcher's D-Bus allowlists, enforced by a real `xdg-dbus-proxy` between a
//! sandboxed client and real buses: what a portal-less graphical app may reach on the
//! session bus — notifications and nothing else — and that the accessibility bus is
//! not there at all, because the session publishes every window's tree itself.
#![allow(
    clippy::unused_self,
    reason = "zbus interface methods are defined on &self"
)]

use std::path::Path;

use launcher::dbus_proxy::session_filter_args;
use tilo_test_env::PrivateBus;
use zbus::blocking::Connection;

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

fn call(
    connection: &Connection,
    destination: &str,
    path: &str,
    interface: &str,
    method: &str,
) -> zbus::Result<zbus::message::Message> {
    connection.call_method(Some(destination), path, Some(interface), method, &())
}

tilo_test_env::scenario! {
    fn an_app_reaches_only_the_bus_names_and_calls_listed_for_it;

    phase a_graphical_app_reaches_no_accessibility_bus;
    {
        let dir = tempfile::tempdir().unwrap();

        // Session bus: an accessibility launcher and an unrelated service, both of which a
        // sandboxed app must stay away from.
        let session = PrivateBus::start_at(&dir.path().join("session"));
        let _a11y_launcher = connect(session.address())
            .name("org.a11y.Bus")
            .unwrap()
            .serve_at("/org/a11y/bus", A11yBus)
            .unwrap()
            .serve_at("/org/a11y/bus", A11yStatus { enabled: true })
            .unwrap()
            .build()
            .unwrap();
        let _other = connect(session.address())
            .name("org.example.Other")
            .unwrap()
            .serve_at("/", Other)
            .unwrap()
            .build()
            .unwrap();

        // A portal-less graphical app: the session filter carries notifications and no
        // accessibility rule at all.
        let filter = session_filter_args(&std::collections::BTreeMap::new(), true, None);
        assert!(
            !filter.iter().any(|rule| rule.contains("org.a11y")),
            "no accessibility rule reaches an app: {filter:?}"
        );
        let session_proxy = Proxy::start(
            session.address(),
            &dir.path().join("session-proxy"),
            &filter.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let app_session = session_proxy.connect();

        // Neither where the accessibility bus is, nor whether it is on: the session publishes
        // every window's tree, so an app has nothing to ask of it. The fixture answers both on the
        // bus itself, so a refusal below is the proxy's.
        let direct = connect(session.address()).build().unwrap();
        let reply = call(
            &direct,
            "org.a11y.Bus",
            "/org/a11y/bus",
            "org.a11y.Bus",
            "GetAddress",
        )
        .expect("the fixture serves the accessibility launcher");
        let address: String = reply.body().deserialize().unwrap();
        assert_eq!(address, "unix:path=/run/user/0/at-spi/bus");
        assert!(
            call(
                &app_session,
                "org.a11y.Bus",
                "/org/a11y/bus",
                "org.a11y.Bus",
                "GetAddress",
            )
            .is_err(),
            "the app must not learn the accessibility bus's address"
        );
        assert!(
            app_session
                .call_method(
                    Some("org.a11y.Bus"),
                    "/org/a11y/bus",
                    Some("org.freedesktop.DBus.Properties"),
                    "Get",
                    &("org.a11y.Status", "IsEnabled"),
                )
                .is_err(),
            "the app must not read whether assistive technology is on"
        );

        // And nothing else on the session bus either.
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
    }

    phase a_media_app_owns_its_own_player_name_and_no_other;
    {
        let dir = tempfile::tempdir().unwrap();
        let session = PrivateBus::start_at(&dir.path().join("session"));
        let player = launcher::dbus_proxy::media_player_name("so.tilo.music").unwrap();
        let filter = session_filter_args(&std::collections::BTreeMap::new(), true, Some(&player));
        let proxy = Proxy::start(
            session.address(),
            &dir.path().join("session-proxy"),
            &filter.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let request = |connection: &Connection, name: &str| {
            connection.request_name_with_flags(name, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        };

        // The app takes its player's name, as the toolkit's media session does, and the desktop's
        // now-playing observer on the real bus finds it there.
        let app = proxy.connect();
        assert_eq!(
            request(&app, &player).expect("the app may own its player name"),
            zbus::fdo::RequestNameReply::PrimaryOwner
        );
        let desktop = connect(session.address()).build().unwrap();
        let owner = zbus::blocking::fdo::DBusProxy::new(&desktop)
            .unwrap()
            .get_name_owner(player.as_str().try_into().unwrap())
            .expect("the player is on the session bus");
        assert_eq!(owner.as_str(), app.unique_name().unwrap().as_str());

        // Another app's player, and a name below its own, stay refused.
        for other in [
            "org.mpris.MediaPlayer2.so.tilo.tube",
            "org.mpris.MediaPlayer2.so.tilo.music.extra",
        ] {
            assert!(request(&app, other).is_err(), "{other} must stay refused");
        }
    }
}

/// The portal frontend's two faces under test, counting the calls that reach it.
struct PortalInterface {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

struct NotificationPortal(PortalInterface);

#[zbus::interface(name = "org.freedesktop.portal.Notification")]
impl NotificationPortal {
    fn add_notification(&self, id: &str) {
        assert_eq!(id, "hello");
        self.0
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

struct ScreenCastPortal(PortalInterface);

#[zbus::interface(name = "org.freedesktop.portal.ScreenCast")]
impl ScreenCastPortal {
    fn create_session(&self) {
        self.0
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The portals `granted` allowed and those in `denied` denied.
fn portals(
    granted: &[&str],
    denied: &[&str],
) -> std::collections::BTreeMap<String, launcher::manifest::PortalPolicy> {
    use launcher::manifest::PortalPolicy;
    let policy = |names: &[&str], policy: PortalPolicy| {
        names
            .iter()
            .map(move |name| ((*name).to_owned(), policy))
            .collect::<Vec<_>>()
    };
    policy(granted, PortalPolicy::ViaHost)
        .into_iter()
        .chain(policy(denied, PortalPolicy::Deny))
        .collect()
}

/// A sandboxed CLI launch of `manifest` from `payload`, its app directory under `data`, with a
/// window on `display` if one is given: what it printed, once it exited successfully.
fn launched(
    manifest: &launcher::manifest::ManifestView,
    payload: &Path,
    data: &Path,
    display: Option<&Path>,
    command: &str,
    argv: &[&str],
) -> String {
    use launcher::run as launch;
    let mut opts = crate::support::isolated_options(data);
    opts.no_services = true;
    opts.bwrap_bin = Some(crate::support::bwrap());
    opts.sandbox_shim_bin = Some(tilo_test_env::package_binary!("tilo-launcher"));
    opts.wayland_socket = display.map(Path::to_path_buf);
    opts.mode = launch::LaunchMode::Cli {
        command: command.to_owned(),
        passthrough_argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
    };
    let (status, out) = launch::launch_with_captured(manifest, payload, &opts).expect("launch");
    let out = String::from_utf8_lossy(&out).trim().to_owned();
    assert!(status.success(), "the app exited {status:?}: {out}");
    out
}

/// A real sandboxed launch of an app whose manifest declares no notifications and denies
/// screencast: a notification call from its window reaches the portal, and a screencast call is
/// refused by the bus proxy before the portal sees it.
#[test]
fn a_launched_app_reaches_only_the_portals_its_manifest_allows() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // The fixture is built before the environment is held: its build takes the environment lock.
    // The fixture with `dbus-send` beside it, the app's own D-Bus client.
    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    for (from, name) in [
        (
            crate::support::workspacetest_payload().join("bin/workspacetest"),
            "workspacetest",
        ),
        (std::path::PathBuf::from("/usr/bin/dbus-send"), "dbus-send"),
    ] {
        std::fs::copy(from, payload.join("bin").join(name)).unwrap();
    }
    let session = PrivateBus::start_at(&dir.path().join("session"));
    let (notified, cast) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let _portal = connect(session.address())
        .name("org.freedesktop.portal.Desktop")
        .unwrap()
        .serve_at(
            "/org/freedesktop/portal/desktop",
            NotificationPortal(PortalInterface {
                calls: notified.clone(),
            }),
        )
        .unwrap()
        .serve_at(
            "/org/freedesktop/portal/desktop",
            ScreenCastPortal(PortalInterface {
                calls: cast.clone(),
            }),
        )
        .unwrap()
        .build()
        .unwrap();
    let runtime = dir.path().join("runtime");
    std::fs::create_dir(&runtime).unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("DBUS_SESSION_BUS_ADDRESS", session.address())
        .set("XDG_RUNTIME_DIR", &runtime);

    let mut manifest = crate::support::manifest("so.tilo.portaltest", "bin/workspacetest", "off");
    manifest.sandbox.portals = portals(&[], &["screencast"]);
    let data = tempfile::tempdir().unwrap();
    let display = dir.path().join("wayland-0");
    let _display = std::os::unix::net::UnixListener::bind(&display).unwrap();
    let call = |method: &str, arg: Option<&str>| {
        let mut argv = vec![
            "exec",
            "dbus-send",
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            method,
        ];
        argv.extend(arg);
        launched(
            &manifest,
            &payload,
            data.path(),
            Some(&display),
            "workspacetest",
            &argv,
        )
    };

    let allowed = call(
        "org.freedesktop.portal.Notification.AddNotification",
        Some("string:hello"),
    );
    assert!(
        allowed.starts_with("EXIT 0"),
        "an app with a window notifies without declaring it: {allowed}"
    );
    assert_eq!(notified.load(Ordering::SeqCst), 1);

    let refused = call("org.freedesktop.portal.ScreenCast.CreateSession", None);
    assert!(
        refused.starts_with("EXIT 1")
            && refused.contains("org.freedesktop.DBus.Error.AccessDenied"),
        "the proxy refuses the denied portal: {refused}"
    );
    assert_eq!(
        cast.load(Ordering::SeqCst),
        0,
        "the denied portal never sees the call"
    );
}

/// The Secret portal's frontend under test: it hands every app the same master secret, as the
/// session's portal hands one app its own, and counts the requests that reach it.
struct SecretPortal {
    asked: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[zbus::interface(name = "org.freedesktop.portal.Secret")]
impl SecretPortal {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }

    async fn retrieve_secret(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        fd: zbus::zvariant::OwnedFd,
        options: std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
    ) -> zbus::zvariant::OwnedObjectPath {
        use std::io::Write as _;
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::fs::File::from(std::os::fd::OwnedFd::from(fd))
            .write_all(&[7; 32])
            .unwrap();
        let sender = header
            .sender()
            .unwrap()
            .trim_start_matches(':')
            .replace('.', "_");
        let token =
            String::try_from(options.get("handle_token").unwrap().try_clone().unwrap()).unwrap();
        let handle = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
        connection
            .emit_signal(
                None::<&str>,
                handle.as_str(),
                "org.freedesktop.portal.Request",
                "Response",
                &(
                    0_u32,
                    std::collections::HashMap::<String, zbus::zvariant::OwnedValue>::new(),
                ),
            )
            .await
            .unwrap();
        zbus::zvariant::OwnedObjectPath::try_from(handle).unwrap()
    }
}

/// The app `a_sandboxed_app_keeps_its_secrets_across_launches` starts in the sandbox: the
/// command its launch names (`put`, `get`) is the step it takes. Started any other way, it does
/// nothing.
#[test]
fn sandboxed_secret_step() {
    let Ok(step) = std::env::var("TILO_CLI_COMMAND") else {
        return;
    };
    let store = tilo_secrets::SecretStore::for_app("so.tilo.secrettest");
    let outcome = match step.as_str() {
        "put" => store
            .put_blocking(
                "token",
                b"kept across launches",
                tilo_secrets::PutOptions::default(),
            )
            .map(|()| {
                // Sealed in the app's directory, with no key beside it.
                let dir = std::env::var("XDG_DATA_HOME").unwrap() + "/tilo-secrets";
                let files: Vec<_> = std::fs::read_dir(dir)
                    .unwrap()
                    .map(|entry| std::fs::read(entry.unwrap().path()).unwrap())
                    .collect();
                let sealed = files.len() == 1 && !files[0].windows(8).any(|w| w == b"kept acr");
                format!("stored, sealed: {sealed}")
            }),
        "get" => store.get_blocking("token").map(|secret| {
            secret.map_or_else(
                || "missing".to_owned(),
                |secret| String::from_utf8_lossy(secret.expose()).into_owned(),
            )
        }),
        _ => return,
    };
    match outcome {
        Ok(text) => println!("SECRET {text}"),
        Err(error) => println!("SECRET ERROR {error}"),
    }
}

/// A real sandboxed launch of an app granted the Secret portal stores a secret, and a second
/// launch reads it back: sealed in the app's own directory under the key the portal hands it,
/// with no key file beside it. An app not granted the portal is refused by the bus proxy and
/// stores nothing.
#[test]
fn a_sandboxed_app_keeps_its_secrets_across_launches() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // This test binary is the app: `sandboxed_secret_step` is its body.
    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::copy(std::env::current_exe().unwrap(), payload.join("bin/app")).unwrap();
    let session = PrivateBus::start_at(&dir.path().join("session"));
    let asked = Arc::new(AtomicUsize::new(0));
    let _portal = connect(session.address())
        .name("org.freedesktop.portal.Desktop")
        .unwrap()
        .serve_at(
            "/org/freedesktop/portal/desktop",
            SecretPortal {
                asked: asked.clone(),
            },
        )
        .unwrap()
        .build()
        .unwrap();
    let runtime = dir.path().join("runtime");
    std::fs::create_dir(&runtime).unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("DBUS_SESSION_BUS_ADDRESS", session.address())
        .set("XDG_RUNTIME_DIR", &runtime);

    let data = tempfile::tempdir().unwrap();
    let step = |portal: &str, step: &str| {
        let mut manifest = crate::support::manifest("so.tilo.secrettest", "bin/app", "off");
        manifest.sandbox.portals = portals(&[portal], &[]);
        let argv = [
            "--exact",
            "bus_filters::sandboxed_secret_step",
            "--nocapture",
        ];
        let out = launched(&manifest, &payload, data.path(), None, step, &argv);
        out.lines()
            .find_map(|line| line.strip_prefix("SECRET "))
            .unwrap_or_else(|| panic!("the app took its step: {out}"))
            .to_owned()
    };

    let refused = step("file-chooser", "put");
    assert!(
        refused.starts_with("ERROR") && refused.contains("AccessDenied"),
        "an app without the Secret portal is refused: {refused}"
    );
    assert_eq!(asked.load(Ordering::SeqCst), 0, "the portal never saw it");

    assert_eq!(step("secrets", "put"), "stored, sealed: true");
    assert_eq!(step("secrets", "get"), "kept across launches");
    assert_eq!(asked.load(Ordering::SeqCst), 2, "each launch asks once");
}
