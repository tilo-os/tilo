//! What the panel, running as its own process, tells assistive technology,
//! and what it says back to it.
//!
//! A panel the session starts is an ordinary app: ui-core's native runner
//! gives it a window, and its AccessKit tree goes out over AT-SPI2 like any
//! app's. These start Quick Settings through the session's surface machinery,
//! join it to a private accessibility bus, and walk what an external reader
//! sees — then let the panel's own reader speak the control it focused.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "a failed step in a test is the assertion, and the session outlives every call"
)]

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use atspi_proxies::accessible::AccessibleProxy;
use atspi_proxies::bus::StatusProxy;
use tilo_compositor::OutputConfig;
use tilo_session::surfaces::{Anchor, Keyboard, Layer, Surface};
use tilo_session::{Options, Session};
use tilo_test_env::{PrivateBus, ScopedEnv};
use zbus::zvariant::{ObjectPath, OwnedObjectPath};

const WIDTH: i32 = 800;
const HEIGHT: i32 = 600;
/// How long the panel may take to show its window and publish itself.
const OPENING: Duration = Duration::from_secs(90);
/// One display's refresh: how often the session asks its clients for their
/// next frame.
const REFRESH: Duration = Duration::from_millis(16);
/// Where every provider keeps its root object.
const ROOT: &str = "/org/a11y/atspi/accessible/root";
/// How many nodes one walk reads, so a provider with a cycle fails instead
/// of hanging the test.
const WALK_LIMIT: usize = 256;

/// The panel, as the session starts it: below the bar's right end.
fn panel() -> Surface {
    Surface::new("quick-settings", panel_binary())
        .layer(Layer::Top)
        .namespace("tilo-quick-settings")
        .anchor(Anchor::TOP | Anchor::RIGHT)
        .size((
            tilo_quick_settings::resource::PANEL_WIDTH,
            u32::try_from(HEIGHT).unwrap(),
        ))
        .keyboard(Keyboard::OnDemand)
}

/// The panel's own binary: this is the package Cargo builds it for, so it is
/// there whether a person runs this package's tests or the whole workspace's.
fn panel_binary() -> PathBuf {
    tilo_test_env::package_binary!("tilo-quick-settings")
}

#[test]
fn a_panel_publishes_its_accessibility_tree() {
    let assistive = Assistive::start();
    let mut session = session_with(&[panel()]);
    shown(&mut session);
    let names = assistive.until_it_sees(&mut session, "Brightness");

    // The controls a person reaches with a screen reader are there, by the
    // names they hear, and they can be pressed through AT-SPI.
    for control in ["Brightness", "Volume", "Night light"] {
        assert!(
            names.iter().any(|name| name == control),
            "the panel's tree names no {control}: {names:?}"
        );
    }
    assert!(
        assistive.pressable("Night light"),
        "the night light tile takes a press over AT-SPI"
    );

    session.stop_surface("quick-settings");
}

#[test]
fn the_reader_speaks_the_focused_panel_control() {
    let spoken = Spoken::start();
    let assistive = Assistive::start();
    let mut session = session_with(&[panel()]);
    shown(&mut session);
    assistive.until_it_sees(&mut session, "Volume");
    assert!(
        !spoken.said().contains("Volume"),
        "the panel speaks what it is focused on, and it starts out focused on nothing: {:?}",
        spoken.said()
    );

    // Assistive technology focuses the volume control the way it acts on
    // any app: the control the reader reached takes the keyboard.
    assert!(
        assistive.focus("Volume"),
        "the panel's volume control takes focus over AT-SPI"
    );

    let deadline = Instant::now() + OPENING;
    while !spoken.said().contains("Volume") {
        assert!(
            Instant::now() < deadline,
            "the panel's reader said nothing about the control it focused: {:?}",
            spoken.said()
        );
        pump(&mut session);
        std::thread::sleep(Duration::from_millis(50));
    }
    session.stop_surface("quick-settings");
}

/// Pump until the panel has listed the actions it offers: it is up, and
/// what it draws is its own frame on the session's display.
fn shown(session: &mut Session) {
    let deadline = Instant::now() + OPENING;
    while session.surfaces().published("quick-settings").is_empty() {
        assert!(
            Instant::now() < deadline,
            "the panel published nothing: {:?}",
            session.surfaces().names()
        );
        pump(session);
    }
}

/// A headless session that starts `surfaces` with it.
fn session_with(surfaces: &[Surface]) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            surfaces: surfaces.to_vec(),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// One turn of the session's loop.
fn pump(session: &mut Session) {
    session.step(REFRESH).expect("a turn of the session");
}

/// The buses a panel's accessibility tree is read over, and the bus
/// services a real session runs for it.
struct Assistive {
    /// The session's bus, where `org.a11y.Bus` says assistive technology is
    /// on.
    session_bus: PrivateBus,
    /// The accessibility bus providers join and readers walk.
    a11y_bus: PrivateBus,
    /// The providers that registered themselves, by the bus name each keeps
    /// its root object on.
    providers: Arc<Mutex<Vec<String>>>,
    /// The services serving those buses, stopped with the buses.
    #[expect(
        dead_code,
        reason = "held for its Drop: the services stop when the test ends"
    )]
    services: Vec<Served>,
    /// Names both buses for every process this test starts, until the test
    /// ends.
    #[expect(
        dead_code,
        reason = "held for its Drop: the environment is put back when the test ends"
    )]
    environment: ScopedEnv,
}

impl Assistive {
    /// Buses of their own, with what a person's session serves on them.
    fn start() -> Self {
        let session_bus = PrivateBus::start();
        let a11y_bus = PrivateBus::start();
        let providers = Arc::new(Mutex::new(Vec::new()));
        let mut environment = ScopedEnv::new();
        // What a person's session bus is to an app, and the accessibility
        // bus a launcher hands a sandboxed app: the session's own, private
        // to this test.
        environment.set("DBUS_SESSION_BUS_ADDRESS", session_bus.address());
        environment.set("AT_SPI_BUS_ADDRESS", a11y_bus.address());
        let services = vec![
            serve_status(session_bus.address()),
            serve_registry(a11y_bus.address(), Arc::clone(&providers)),
        ];
        Self {
            session_bus,
            a11y_bus,
            providers,
            services,
            environment,
        }
    }

    /// Say on the session bus whether assistive technology is on, the way a
    /// screen reader does.
    fn say_assistive_technology(&self, on: bool) {
        block_on(async {
            let status = StatusProxy::new(&connected(self.session_bus.address()).await)
                .await
                .expect("the session bus serves org.a11y.Bus");
            status.set_is_enabled(on).await.expect("IsEnabled is set");
        });
    }

    /// Every name an external reader reaches from the newest provider's
    /// root.
    fn reader_sees(&self) -> Vec<String> {
        self.newest().map_or_else(Vec::new, |provider| {
            walk(self.a11y_bus.address(), &provider)
        })
    }

    /// The bus name of the provider that embedded last: the panel this test
    /// started.
    fn newest(&self) -> Option<String> {
        self.providers
            .lock()
            .expect("the providers")
            .last()
            .cloned()
    }

    /// Say assistive technology is on until the newest provider's tree
    /// names `control`, which proves the panel is up and reachable.
    ///
    /// An AccessKit provider joins the accessibility bus only once assistive
    /// technology says it is on, and it wakes on that change rather than on
    /// the value it finds. Say it until the provider hears it: it may still
    /// be connecting the first time.
    fn until_it_sees(&self, session: &mut Session, control: &str) -> Vec<String> {
        let deadline = Instant::now() + OPENING;
        loop {
            self.say_assistive_technology(true);
            pump(session);
            let names = self.reader_sees();
            if names.iter().any(|name| name == control) {
                return names;
            }
            assert!(
                Instant::now() < deadline,
                "the panel published no accessibility tree: {names:?}"
            );
            self.say_assistive_technology(false);
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The provider and object path of the control named `name`.
    fn node(&self, name: &str) -> Option<(String, String)> {
        let provider = self.newest()?;
        block_on(async {
            let connection = connected(self.a11y_bus.address()).await;
            let mut queue = vec![ROOT.to_owned()];
            let mut seen = HashSet::new();
            while let Some(path) = queue.pop() {
                if !seen.insert(path.clone()) || seen.len() > WALK_LIMIT {
                    continue;
                }
                let Ok(node) = accessible(&connection, &provider, &path).await else {
                    continue;
                };
                if node.name().await.is_ok_and(|named| named == name) {
                    return Some((provider.clone(), path));
                }
                if let Ok(children) = node.get_children().await {
                    queue.extend(children.iter().map(|child| child.path_as_str().to_owned()));
                }
            }
            None
        })
    }

    /// The verbs a control named `name` offers over AT-SPI.
    fn verbs(&self, name: &str) -> Vec<String> {
        let Some((provider, path)) = self.node(name) else {
            return Vec::new();
        };
        block_on(verbs(self.a11y_bus.address(), &provider, &path))
    }

    /// Whether a control named `name` can be pressed over AT-SPI.
    fn pressable(&self, name: &str) -> bool {
        self.verbs(name).iter().any(|verb| verb == "click")
    }

    /// Move the keyboard to the control named `name`, the way an external
    /// reader does when it reaches a control and acts on it. Whether the
    /// panel took it.
    fn focus(&self, name: &str) -> bool {
        let Some((provider, path)) = self.node(name) else {
            return false;
        };
        block_on(async {
            let connection = connected(self.a11y_bus.address()).await;
            let Ok(component) = atspi_proxies::component::ComponentProxy::builder(&connection)
                .destination(provider)
                .expect("a bus name")
                .path(path)
                .expect("an object path")
                .build()
                .await
            else {
                return false;
            };
            component.grab_focus().await.unwrap_or(false)
        })
    }
}

/// The verbs the node at `path` offers, by the names a reader sees.
async fn verbs(address: &str, provider: &str, path: &str) -> Vec<String> {
    let connection = connected(address).await;
    verbs_over(&connection, provider, path)
        .await
        .unwrap_or_default()
}

/// The verbs the node at `path` offers, over a connection already open.
async fn verbs_over(
    connection: &zbus::Connection,
    provider: &str,
    path: &str,
) -> Result<Vec<String>, zbus::Error> {
    let actions = atspi_proxies::action::ActionProxy::builder(connection)
        .destination(provider.to_owned())?
        .path(path.to_owned())?
        .build()
        .await?
        .get_actions()
        .await?;
    Ok(actions.into_iter().map(|action| action.name).collect())
}

/// Where a panel's reader speaks: a stand-in `espeak-ng` on the processes'
/// `PATH` that keeps what it is asked to say.
///
/// The reader reaches for a speech stack outside its process, speech-dispatcher
/// first and `espeak-ng` after it. An empty runtime directory leaves no
/// speech-dispatcher socket to find, so the panel falls through to the
/// `espeak-ng` this test writes, and the words it is handed here are what the
/// panel said.
struct Spoken {
    /// The stand-in and the file it appends to.
    #[expect(
        dead_code,
        reason = "held for its Drop: the stand-in is removed when the test ends"
    )]
    directory: tempfile::TempDir,
    /// Everything said, one utterance a line.
    words: PathBuf,
    /// The `PATH` and runtime directory every process this test starts reads
    /// until the test ends, and the reader turned on for the panel.
    #[expect(
        dead_code,
        reason = "held for its Drop: the environment is put back when the test ends"
    )]
    environment: ScopedEnv,
}

impl Spoken {
    fn start() -> Self {
        let directory = tempfile::tempdir().expect("a directory for the stand-in");
        let words = directory.path().join("said");
        let espeak = directory.path().join("espeak-ng");
        std::fs::write(
            &espeak,
            format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = --version ]; then exit 0; fi\n\
                 cat >> {0}\n\
                 printf '\\n' >> {0}\n",
                words.display()
            ),
        )
        .expect("the stand-in's script");
        std::fs::set_permissions(&espeak, std::fs::Permissions::from_mode(0o755))
            .expect("the stand-in is runnable");
        let mut environment = ScopedEnv::new();
        let path = std::env::var("PATH").unwrap_or_default();
        environment.set("PATH", format!("{}:{path}", directory.path().display()));
        // Where speech-dispatcher's socket would be looked for: nowhere, so
        // the reader speaks through `espeak-ng`.
        environment.set("XDG_RUNTIME_DIR", directory.path().display().to_string());
        // A panel reads this at startup, the way a login screen reads the
        // device's sign-in comforts.
        environment.set("TILO_SCREEN_READER", "1");
        Self {
            directory,
            words,
            environment,
        }
    }

    /// Everything the reader has spoken so far.
    fn said(&self) -> String {
        std::fs::read_to_string(&self.words).unwrap_or_default()
    }
}

/// Every accessible name from `provider`'s root down.
fn walk(address: &str, provider: &str) -> Vec<String> {
    block_on(async {
        let connection = connected(address).await;
        let mut queue = vec![ROOT.to_owned()];
        let mut seen = HashSet::new();
        let mut names = Vec::new();
        while let Some(path) = queue.pop() {
            if !seen.insert(path.clone()) || seen.len() > WALK_LIMIT {
                continue;
            }
            let Ok(node) = accessible(&connection, provider, &path).await else {
                continue;
            };
            if let Ok(name) = node.name().await {
                names.push(name);
            }
            if let Ok(children) = node.get_children().await {
                queue.extend(children.iter().map(|child| child.path_as_str().to_owned()));
            }
        }
        names
    })
}

/// One node of a provider's tree.
async fn accessible<'a>(
    connection: &'a zbus::Connection,
    provider: &str,
    path: &str,
) -> Result<AccessibleProxy<'a>, zbus::Error> {
    AccessibleProxy::builder(connection)
        .destination(provider.to_owned())?
        .path(path.to_owned())?
        .build()
        .await
}

/// A connection to the bus at `address`.
async fn connected(address: &str) -> zbus::Connection {
    zbus::connection::Builder::address(address)
        .expect("a bus address")
        .build()
        .await
        .expect("the bus accepts a client")
}

/// Run `future` on a runtime of its own, so a test's calls never wait on
/// another test's.
fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(future)
}

/// Serve what a session's bus says about assistive technology.
fn serve_status(address: &str) -> Served {
    serve(address, |server, connection| async move {
        server
            .at("/org/a11y/bus", Status::default())
            .await
            .expect("the status interface");
        connection
            .request_name("org.a11y.Bus")
            .await
            .expect("the name a reader looks up");
    })
}

/// Serve the registry providers register with.
fn serve_registry(address: &str, providers: Arc<Mutex<Vec<String>>>) -> Served {
    serve(address, move |server, connection| {
        let registry = Registry {
            name: connection
                .unique_name()
                .map(ToString::to_string)
                .unwrap_or_default(),
            providers,
        };
        async move {
            server
                .at(ROOT, registry)
                .await
                .expect("the registry's root");
            connection
                .request_name("org.a11y.atspi.Registry")
                .await
                .expect("the name providers embed with");
        }
    })
}

/// A bus service running on a thread of its own, stopped when dropped.
struct Served {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Served {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Connect to `address`, add what `interfaces` puts on the object server,
/// and stay serving it until the service is dropped.
fn serve<F, Fut>(address: &str, interfaces: F) -> Served
where
    F: FnOnce(zbus::ObjectServer, zbus::Connection) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let address = address.to_owned();
    let (ready, served) = std::sync::mpsc::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        runtime.block_on(async move {
            let connection = connected(&address).await;
            // The connection stays open while the service runs: the
            // interfaces only borrow it.
            interfaces(connection.object_server().clone(), connection.clone()).await;
            ready.send(()).expect("the test waits for this");
            let _ = stopped.await;
        });
    });
    served.recv().expect("the service is up");
    Served {
        stop: Some(stop),
        thread: Some(thread),
    }
}

/// What a session bus says about assistive technology: `org.a11y.Status` at
/// the bus's own object.
#[derive(Debug, Default)]
struct Status {
    enabled: bool,
}

#[zbus::interface(name = "org.a11y.Status")]
impl Status {
    #[zbus(property, name = "IsEnabled")]
    fn is_enabled(&self) -> bool {
        self.enabled
    }

    #[zbus(property, name = "IsEnabled")]
    fn set_is_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// A reader is told whether a screen reader is on; this test only
    /// drives the one flag a provider wakes on.
    #[zbus(property, name = "ScreenReaderEnabled")]
    #[allow(
        clippy::unused_self,
        reason = "the interface asks a property for the object it is on"
    )]
    fn screen_reader_enabled(&self) -> bool {
        false
    }
}

/// The registry providers embed with: it answers with the root object they
/// hang their tree under, and remembers who registered.
#[derive(Debug)]
struct Registry {
    /// The registry's bus name, which a provider's tree names as its
    /// desktop.
    name: String,
    /// Every provider that registered, newest last.
    providers: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.a11y.atspi.Socket")]
impl Registry {
    /// `Embed(s o)` hands the plug's bus name and root object over as the
    /// interface's signature says; it is not ours to take by reference.
    #[zbus(name = "Embed")]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the interface's own signature takes the plug by value"
    )]
    fn embed(&self, plug: (&str, ObjectPath<'_>)) -> (String, OwnedObjectPath) {
        self.providers
            .lock()
            .expect("the providers")
            .push(plug.0.to_owned());
        (
            self.name.clone(),
            OwnedObjectPath::try_from(ROOT).expect("the root path"),
        )
    }
}
