//! The desktop services the session starts: the portal backend and the
//! notifications daemon on the session bus, the polkit, network secrets
//! and Bluetooth pairing agents on the system bus.
//!
//! They speak D-Bus on a runtime of their own and publish beside the
//! system services: everything that needs the person's answer is a
//! question resource (`question:<handle>`) the sheets answer, and the live
//! notifications are resources the banners and the centre read. None of
//! them draws anything.

use std::sync::Arc;

use tilo_lock::MutexRecover as _;
use tilo_network_secret_agent::store::{RoamingWifiSlot, VaultStore};
use tilo_portal::{Portal, PortalAuthenticator, PortalCaller, Requesters, settings};
use tilo_service::{Questions, Service};

use crate::portal_link::PortalSide;

/// How long the session waits for the monitors to answer before it says
/// it ends, and how often it asks.
const QUERY_END_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
const QUERY_END_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Where a bus is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Bus {
    /// Not reached: what needs it does not start.
    None,
    /// The bus the environment names, as `DBUS_SESSION_BUS_ADDRESS` (or
    /// the system bus's well-known socket) does.
    #[default]
    Default,
    /// The bus at this address.
    Address(String),
}

/// The buses the desktop services serve on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Buses {
    /// The portal backend and the notifications daemon serve here.
    pub session: Bus,
    /// The polkit, network secrets and Bluetooth agents register here.
    pub system: Bus,
}

/// The desktop services while they run; they stop when it drops.
pub struct DesktopServices {
    runtime: tokio::runtime::Runtime,
    questions: Questions,
    portal: Option<Portal>,
    /// The files people choose for apps, while they are served as a
    /// filesystem: the same table the portal grants through, which a
    /// launcher binds an app's view of and the app finds at its own
    /// documents directory. Dropping it unmounts.
    chosen: Option<tilo_documents::mount::Mounted>,
    notifications: tilo_notifications_service::Notifications,
    roaming: RoamingWifiSlot,
    /// The session bus connection the portal and the daemon serve on, and
    /// what keeps the daemon's tasks alive.
    session: Option<(zbus::Connection, tilo_notifications_service::Server)>,
    /// The applications' tray items, and what keeps their watcher served.
    tray: Option<(
        Arc<tilo_tray_host::Tray>,
        tilo_tray_host::watcher::WatcherHandle,
    )>,
    /// The apps the session sees run, by process: what names a notification's
    /// sender, shared with the daemon.
    running: Arc<std::sync::Mutex<std::collections::BTreeMap<u32, String>>>,
    /// The system bus the agents registered on, which everything else the
    /// session asks the machine goes through: one connection, not one per
    /// service.
    system: Option<zbus::Connection>,
}

impl DesktopServices {
    /// Start every desktop service `buses` reach, the portal taking its
    /// screenshots with `capture`, sharing the screens, driving the seat and
    /// passing the selection through `side` (the session's own side of it),
    /// Pass the selection through `side` (the session's own side of it),
    /// and take the holds apps ask it for in `inhibitors` (the session's
    /// own, so they keep the machine awake). What people choose for apps is
    /// served as a filesystem here too, from the table the portal grants
    /// through. One that cannot start (no bus, the name taken, no daemon
    /// behind it) is logged and the rest carry on; the session bus services
    /// are serving when this returns.
    ///
    /// # Errors
    /// The runtime they run on could not be made.
    pub fn start(
        buses: &Buses,
        wake: &tilo_service::Wake,
        capture: Arc<dyn tilo_portal::Capture>,
        side: &PortalSide,
        inhibitors: Option<tilo_power_mgmt::InhibitRegistry>,
    ) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("tilo-desktop-services")
            .enable_all()
            .build()?;
        let questions = Questions::new();
        let woken = Arc::clone(wake);
        questions.on_change(move || woken());
        let notifications = tilo_notifications_service::Notifications::for_session();
        let roaming = RoamingWifiSlot::default();
        let running = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
        // One table of chosen files: what the portal grants through, and
        // what the filesystem a launcher binds an app's view of serves.
        let documents = tilo_portal::documents_store();
        let chosen = serve_documents(documents.clone());
        let (portal, session) = runtime.block_on(on_session_bus(OnSessionBus {
            bus: &buses.session,
            questions: &questions,
            notifications: &notifications,
            documents,
            wake: Arc::clone(wake),
            capture,
            side,
            inhibitors,
            running: Arc::clone(&running),
        }));
        let tray = runtime.block_on(tray_on(&buses.session, Arc::clone(&running)));
        let system = match buses.system {
            Bus::None => None,
            _ => runtime.block_on(connect(&buses.system, true)),
        };
        if let Some(connection) = system.clone() {
            runtime.spawn(on_system_bus(
                connection,
                questions.clone(),
                roaming.clone(),
            ));
        }
        Ok(Self {
            runtime,
            questions,
            portal,
            chosen,
            notifications,
            roaming,
            session,
            tray,
            running,
            system,
        })
    }

    /// The questions the services ask the person.
    #[must_use]
    pub const fn questions(&self) -> &Questions {
        &self.questions
    }

    /// The notifications the session shows: what this desktop tells the
    /// person itself goes through the same service as an app's.
    #[must_use]
    pub const fn notifications(&self) -> &tilo_notifications_service::Notifications {
        &self.notifications
    }

    /// What they publish, for the session to host beside its system
    /// services: the questions, the portal's access decisions and holds,
    /// and the live notifications.
    #[must_use]
    pub fn services(&self) -> Vec<Box<dyn Service>> {
        let mut services: Vec<Box<dyn Service>> = vec![
            Box::new(self.questions.clone()),
            Box::new(self.notifications.clone()),
        ];
        if let Some(portal) = &self.portal {
            services.push(Box::new(portal.clone()));
        }
        if let Some((tray, _)) = &self.tray {
            services.push(Box::new(tilo_tray_host::service::TrayService::new(
                Arc::clone(tray),
            )));
        }
        services
    }

    /// The applications' tray items, once their watcher serves; the bar
    /// draws them and the person's clicks go back through it.
    #[must_use]
    pub fn tray(&self) -> Option<&Arc<tilo_tray_host::Tray>> {
        self.tray.as_ref().map(|(tray, _)| tray)
    }

    /// The portal backend, once it serves.
    #[must_use]
    pub const fn portal(&self) -> Option<&Portal> {
        self.portal.as_ref()
    }

    /// Whether the files people choose for apps are served as a filesystem
    /// at [`tilo_documents::mountpoint`], which a launcher binds an app's
    /// view of: only then is a sandboxed app answered with a path its own
    /// sandbox holds the file at. Without it the chooser answers with the
    /// host path, which a sandbox cannot open.
    #[must_use]
    pub const fn documents_served(&self) -> bool {
        self.chosen.is_some()
    }

    /// An app exited: what it was granted while it ran is forgotten, and
    /// its processes no longer name its notifications.
    pub fn app_exited(&self, app_id: &str) {
        if let Some(portal) = &self.portal {
            portal.grants().app_exited(app_id);
        }
        self.running
            .lock_recover()
            .retain(|_, running| running != app_id);
    }

    /// The app `pid` shows a window for, as the session sees it: what a
    /// notification its process sends is attributed to.
    pub fn note_running_app(&self, pid: u32, app_id: &str) {
        let mut running = self.running.lock_recover();
        if running.get(&pid).is_none_or(|known| known != app_id) {
            running.insert(pid, app_id.to_owned());
        }
    }

    /// The screen locked or unlocked: what apps were capturing is revoked
    /// on the lock (nothing recorded is the lock screen), and the
    /// monitors hear of the screensaver either way.
    pub fn set_screen_locked(&self, locked: bool) {
        if locked && let Some(portal) = &self.portal {
            portal.captures().broker().lock_recover().screen_locked();
        }
        self.set_session_state(locked, tilo_portal::SessionState::Running);
    }

    /// The session is ending: apps are asked to wrap up, then told it ends.
    pub fn end(&self) {
        let (Some(portal), Some((connection, _))) = (&self.portal, &self.session) else {
            return;
        };
        let (portal, connection) = (portal.clone(), connection.clone());
        self.runtime.spawn(async move {
            portal
                .set_session_state(&connection, false, tilo_portal::SessionState::QueryEnd)
                .await;
            let asked = tokio::time::Instant::now();
            while portal.awaiting_query_end() > 0 && asked.elapsed() < QUERY_END_WAIT {
                tokio::time::sleep(QUERY_END_POLL).await;
            }
            portal
                .set_session_state(&connection, false, tilo_portal::SessionState::Ending)
                .await;
        });
    }

    fn set_session_state(&self, screensaver_active: bool, state: tilo_portal::SessionState) {
        let (Some(portal), Some((connection, _))) = (&self.portal, &self.session) else {
            return;
        };
        let (portal, connection) = (portal.clone(), connection.clone());
        self.runtime.spawn(async move {
            portal
                .set_session_state(&connection, screensaver_active, state)
                .await;
        });
    }

    /// Where the network secrets agent finds the Wi-Fi passphrases that
    /// roam with the person, once peer sync fills it.
    #[must_use]
    pub const fn roaming_wifi(&self) -> &RoamingWifiSlot {
        &self.roaming
    }

    /// Tell the portal which apps run now, each running in the background,
    /// with a window, or in use (`tilo_portal::background`'s states), so
    /// the frontend can tell an app that went windowless; the frontend is
    /// told when that changed.
    pub fn set_running_apps(&self, apps: impl IntoIterator<Item = (String, u32)>) {
        let Some(portal) = &self.portal else {
            return;
        };
        if !portal.app_states().set(apps) {
            return;
        }
        if let Some((connection, _)) = &self.session {
            let connection = connection.clone();
            self.runtime.spawn(async move {
                if let Err(error) =
                    tilo_portal::background::Background::running_changed(&connection).await
                {
                    tracing::debug!(%error, "running apps not announced");
                }
            });
        }
    }

    /// The runtime they run on.
    #[must_use]
    pub const fn runtime(&self) -> &tokio::runtime::Runtime {
        &self.runtime
    }

    /// The system bus the agents registered on: what else the session asks
    /// the machine goes through the same connection.
    #[must_use]
    pub fn system_bus(&self) -> Option<&zbus::Connection> {
        self.system.as_ref()
    }
}

/// Serve the files people choose for apps as a filesystem at
/// [`tilo_documents::mountpoint`], the table `documents` is: what the portal
/// grants through and what a launcher binds an app's view of.
///
/// `None` where it cannot be served (no runtime directory, no FUSE): the
/// portal is still told what apps run, but a chooser answering a sandboxed
/// app then names the host file itself, which the app's sandbox cannot open.
fn serve_documents(documents: tilo_documents::Documents) -> Option<tilo_documents::mount::Mounted> {
    let Some(at) = tilo_documents::mountpoint() else {
        tracing::warn!(
            "no runtime directory, so the files people choose for apps are not served: \
             a sandboxed app is handed the host path of the file it picks"
        );
        return None;
    };
    match tilo_documents::mount::mount(documents, &at) {
        Ok(mounted) => {
            tracing::info!(at = %at.display(), "serving the files people choose for apps");
            Some(mounted)
        }
        Err(error) => {
            tracing::warn!(
                %error,
                at = %at.display(),
                "the files people choose for apps are not served: \
                 a sandboxed app is handed the host path of the file it picks"
            );
            None
        }
    }
}

async fn connect(bus: &Bus, system: bool) -> Option<zbus::Connection> {
    let connected = match bus {
        Bus::None => return None,
        Bus::Default if system => zbus::Connection::system().await,
        Bus::Default => zbus::Connection::session().await,
        Bus::Address(address) => match zbus::connection::Builder::address(address.as_str()) {
            Ok(builder) => builder.build().await,
            Err(error) => Err(error),
        },
    };
    connected
        .inspect_err(|error| {
            let bus = if system { "system" } else { "session" };
            tracing::warn!(%error, bus, "desktop services cannot reach the bus");
        })
        .ok()
}

/// The watcher of the applications' tray items, serving on `bus`. An item
/// that names its icon rather than sending it is drawn from the person's
/// icon themes, and the app that registered it is named from its process:
/// the bar shows and words an item as the app's own. Nothing is dropped for
/// an icon that cannot be found — the host falls back to a letter badge.
async fn tray_on(
    bus: &Bus,
    running: Arc<std::sync::Mutex<std::collections::BTreeMap<u32, String>>>,
) -> Option<(
    Arc<tilo_tray_host::Tray>,
    tilo_tray_host::watcher::WatcherHandle,
)> {
    let builder = match bus {
        Bus::None => return None,
        Bus::Default => zbus::connection::Builder::session(),
        Bus::Address(address) => zbus::connection::Builder::address(address.as_str()),
    };
    let tray = tilo_tray_host::Tray::with_owner_resolver(
        Arc::new(crate::app_icons::tray_raster),
        tray_owner_resolver(running),
    );
    let served = match builder {
        Ok(builder) => tilo_tray_host::watcher::serve(builder, Arc::clone(&tray)).await,
        Err(error) => Err(error),
    };
    served
        .inspect_err(|error| tracing::warn!(%error, "the tray watcher did not start"))
        .ok()
        .map(|watcher| (tray, watcher))
}

/// The app the process `pid` demonstrably belongs to, proven as a
/// notification's sender is: the Flatpak whose sandbox it runs in, else the
/// app the session sees a window for it. A peer-to-peer item proves
/// nothing, and an item's own claim is never taken for it.
fn tray_owner_resolver(
    running: Arc<std::sync::Mutex<std::collections::BTreeMap<u32, String>>>,
) -> tilo_tray_host::OwnerResolver {
    Arc::new(move |pid| {
        tilo_notifications_service::verified_flatpak_app_id(pid)
            .or_else(|| running.lock_recover().get(&pid).cloned())
    })
}

/// What the session bus services serve on, and with.
struct OnSessionBus<'a> {
    /// The bus they serve on.
    bus: &'a Bus,
    questions: &'a Questions,
    notifications: &'a tilo_notifications_service::Notifications,
    /// The files people choose for apps: the table the portal grants
    /// through, which the session serves as a filesystem.
    documents: tilo_documents::Documents,
    /// What the session's loop hears of a change in either service.
    wake: tilo_service::Wake,
    capture: Arc<dyn tilo_portal::Capture>,
    side: &'a PortalSide,
    inhibitors: Option<tilo_power_mgmt::InhibitRegistry>,
    running: Arc<std::sync::Mutex<std::collections::BTreeMap<u32, String>>>,
}

/// The portal backend and the notifications daemon, serving on the bus
/// `serving` names.
async fn on_session_bus(
    serving: OnSessionBus<'_>,
) -> (
    Option<Portal>,
    Option<(zbus::Connection, tilo_notifications_service::Server)>,
) {
    let OnSessionBus {
        bus,
        questions,
        notifications,
        documents,
        wake,
        capture,
        side,
        inhibitors,
        running,
    } = serving;
    let Some(connection) = connect(bus, false).await else {
        return (None, None);
    };
    // The frontend vouches for the app id it forwards; the app is the one
    // it names, in the workspace the session sees it run in.
    let named = side.clone();
    let resolver = Arc::new(move |app_id: &str, _window: Option<&str>| {
        Some(PortalCaller {
            workspace: named.workspace_of(app_id),
            sandboxed: named.sandboxed(app_id),
            ..PortalCaller::new(app_id)
        })
    });
    let portal = Portal::new(
        questions.clone(),
        // An app the frontend could not name is named by its process.
        PortalAuthenticator::session_bus(connection.clone(), resolver)
            .with_identify(Requesters::host().resolver()),
        documents,
    )
    .with_capture(capture)
    // What shares the session's screens, drives its seat and passes its
    // selection.
    .with_streams(side.streams())
    .with_input(side.input())
    .with_clipboard(side.clipboard())
    // A workspace already granting an app what it asks for answers for the
    // person.
    .with_consent_grants(side.grants());
    let portal = match inhibitors {
        Some(registry) => portal.with_inhibitions(registry),
        None => portal,
    };
    // The appearance is also written to dconf, the environment's own: only
    // a portal on the environment's bus speaks for it.
    if *bus == Bus::Default {
        settings::follow_appearance(portal.settings().clone());
    }
    // What the portal publishes of the live sessions changes outside the
    // loop's turns: hearing of it lands on the next one.
    let waking = Arc::clone(&wake);
    portal.sessions().on_change(move || waking());
    let portal = match portal.serve(&connection).await {
        Ok(()) => Some(portal),
        Err(error) => {
            tracing::warn!(%error, "the portal backend did not start");
            None
        }
    };
    // What the daemon receives wakes the loop: the session's banners read
    // the arrivals on its next turn and show them as banners and in the
    // centre, pixels and all; a fresh one does not wait for the loop's pass.
    let woken = Arc::clone(&wake);
    notifications.on_change(move || woken());
    // Sandboxed apps' notifications are the daemon's, shown the same way.
    if let Some(portal) = &portal
        && let Err(error) =
            tilo_portal::notification::serve_on(portal, &connection, notifications.clone()).await
    {
        tracing::warn!(%error, "the notification portal did not start");
    }
    let daemon = tilo_notifications_service::serve_on(
        connection.clone(),
        notifications.clone(),
        Arc::new(crate::notifications::AppCallers::new(running, side.clone())),
    )
    .await;
    let daemon = match daemon {
        Ok(daemon) => match connection
            .request_name(tilo_notifications_service::BUS_NAME)
            .await
        {
            Ok(()) => Some(daemon),
            Err(error) => {
                tracing::warn!(%error, "another notifications daemon owns the name");
                None
            }
        },
        Err(error) => {
            tracing::warn!(%error, "the notifications daemon did not start");
            None
        }
    };
    (portal, daemon.map(|daemon| (connection, daemon)))
}

/// The polkit, network secrets and Bluetooth pairing agents on `connection`,
/// kept registered while the runtime runs.
async fn on_system_bus(
    connection: zbus::Connection,
    questions: Questions,
    roaming: RoamingWifiSlot,
) {
    let polkit = tilo_polkit_agent::questions::serve_on(connection.clone(), questions.clone())
        .await
        .inspect_err(|error| tracing::warn!(%error, "the polkit agent did not start"))
        .ok();
    let network = network_secrets(connection.clone(), questions.clone(), roaming)
        .await
        .inspect_err(|error| tracing::warn!(%error, "the network secrets agent did not start"))
        .ok();
    let client =
        tilo_bluez::BluezClient::with_connection(connection, tilo_bluez::DeviceNames::new());
    let bluetooth = tilo_bluetooth_agent::PairingService::start_with(
        client,
        questions,
        tilo_bluez::DEFAULT_ANSWER_TIMEOUT,
    )
    .await
    .inspect_err(|error| tracing::warn!(%error, "the Bluetooth pairing agent did not start"))
    .ok();
    // Each agent stays registered while its handle lives.
    std::future::pending::<()>().await;
    drop((polkit, network, bluetooth));
}

/// The network secrets agent on `connection`, keeping what the person lets
/// it remember in this session's vault.
async fn network_secrets(
    connection: zbus::Connection,
    questions: Questions,
    roaming: RoamingWifiSlot,
) -> zbus::Result<tilo_network_secret_agent::service::AgentHandle> {
    // Choosing the vault's custody probes the secret store over a blocking
    // D-Bus call, which must not run on a runtime thread.
    let store = tokio::task::spawn_blocking(move || VaultStore::for_session(roaming))
        .await
        .map_err(|error| zbus::Error::Failure(format!("network secrets vault: {error}")))?
        .ok_or_else(|| {
            zbus::Error::Failure("no configuration directory for the network secrets vault".into())
        })?;
    tilo_network_secret_agent::questions::serve_on(
        connection,
        tilo_network_secret_agent::questions::QuestionSheets::new(questions),
        Arc::new(store),
    )
    .await
}
