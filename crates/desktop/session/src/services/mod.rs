//! Every service this session serves, in one table — the name a client dials it by, how the client
//! reaches it, and what serves it — and the app wire the apps read them on ([`app_wire`]).
//!
//! The session's apps and the person's own devices reach it in two ways. A
//! socket under the launcher's services root is what the launcher hands an
//! app at launch and what a program of this login dials by name
//! ([`launcher::services::socket_for`]): one name, one socket, one server.
//! Everything else is a registration: a resource the apps reach on the
//! app wire they already speak on, or, for the work no one calls at
//! all, a worker that counts into the person's own records.
//!
//! [`TABLE`] is the whole answer to "does this session serve X". The
//! services this module starts start from [`start`], which walks the table,
//! and the match over [`Service`] is exhaustive: a row this module owns
//! without code to start it does not compile. The rows served by a module
//! beside this one name that module, and the rows another program of this
//! login serves name the program, so who serves what is read here rather
//! than found by an app whose service is missing.
//!
//! The test below pairs the table with what clients dial — the names the launcher hands an app
//! ([`launcher::services::HANDED_SERVICES`]), the services every app's manifest declares, and the
//! connections every tilolang instance is handed — and fails when one has no row here.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;
use tilo_permissions::caps::DeviceCap;

use serde_json::{Map, Value};
use settings_sections::network_usage::{ConnectionKind, Reading};
use tilo_portal::PortalCaller;
use tilo_service::{AskSender, Services};
use tilo_system_stats::Grant;
use tilo_system_stats::hub::Hub;
use tilo_system_stats::protocol::Handoff;
use tilo_system_stats::server::{Prompter, Server};

use crate::render;
use crate::tools;

pub(crate) mod accounts;
pub(crate) mod app_wire;
pub(crate) mod chooser;
pub(crate) mod clipboard;
pub(crate) mod notify;

/// How a client reaches a service of this session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reach {
    /// On the launcher's socket for it, under the services root
    /// ([`launcher::services::socket_for`]): what it hands an app at launch,
    /// and what a program of this login dials by name.
    Launcher,
    /// On the app wire the session's apps speak on: a resource published beside the others, or
    /// a verb of the person's own records.
    AppWire,
    /// Nowhere: a package names the service in its manifest for the grant it carries, and there is
    /// no connection.
    NoConnection,
}

/// What serves a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    /// This module, from [`start`].
    Session(Service),
    /// The module of the session beside this one, named as the reader finds it.
    Module(&'static str),
    /// Another program of this login.
    Elsewhere(&'static str),
}

/// The services [`start`] starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Service {
    /// The identity broker apps sign in through.
    Identity,
    /// The machine's readings, for the apps the launcher connects and the person grants.
    SystemInfo,
    /// This device's data usage, counted into the person's settings.
    DataUsage,
    /// The typed opens an app asks this session for: a tool at a folder, the person's drives and
    /// apps, a workspace settings tab.
    Tools,
    /// The apps' render services: a toolkit app hands its frame to the service of its workspace,
    /// which draws it on the GPU.
    Render,
    /// The Windows compatibility service the Windows Apps manager sets up and starts Windows
    /// programs through ([`crate::windows_compat`]).
    Windows,
    /// The connections adding an account makes to its servers, made for Settings, whose sandbox
    /// has no network ([`accounts`]).
    Accounts,
    /// The files an app asks the person for, chosen in the session's own file chooser, and the
    /// links and files it opens, in the app that takes them ([`chooser`]).
    Chooser,
    /// The person's clipboard, for the apps that hold no display of their own to offer or read a
    /// selection on ([`clipboard`]).
    Clipboard,
    /// The notifications apps show, and what the person does with them, served to the apps the
    /// launcher connects ([`notify`]).
    Notify,
}

/// One service of this desktop: its name, how it is reached, and what serves it.
pub(crate) struct Row {
    pub name: &'static str,
    pub reach: Reach,
    pub owner: Owner,
}

/// Every service this session's apps and devices reach.
pub(crate) const TABLE: &[Row] = &[
    Row {
        name: launcher::services::COLLAB_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Module("services::app_wire"),
    },
    Row {
        name: launcher::services::SHELL_UI_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Module("app_tabs"),
    },
    Row {
        name: launcher::services::IDENTITY_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Session(Service::Identity),
    },
    Row {
        name: tilo_system_stats::SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Session(Service::SystemInfo),
    },
    Row {
        name: launcher::services::SYSTEM_SERVICE,
        reach: Reach::NoConnection,
        owner: Owner::Module("services::app_wire"),
    },
    Row {
        name: "network-usage",
        reach: Reach::AppWire,
        owner: Owner::Session(Service::DataUsage),
    },
    Row {
        name: launcher::services::CHOOSER_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Session(Service::Chooser),
    },
    Row {
        name: launcher::services::CLIPBOARD_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Session(Service::Clipboard),
    },
    Row {
        name: launcher::services::NOTIFY_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Session(Service::Notify),
    },
    Row {
        name: launcher::services::COMPAT_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Session(Service::Windows),
    },
    Row {
        name: tools::SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Session(Service::Tools),
    },
    Row {
        name: tilo_render_protocol::broker::SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Session(Service::Render),
    },
    Row {
        name: launcher::services::ACCOUNTS_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Session(Service::Accounts),
    },
    Row {
        name: launcher::services::NATIVE_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Elsewhere("the device daemon (tilo-runtime)"),
    },
    Row {
        name: launcher::services::MESSAGING_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Elsewhere("tilo-messaging"),
    },
    Row {
        name: launcher::services::MODEL_GATEWAY_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Elsewhere("tilo-model-gateway"),
    },
    Row {
        name: "models",
        reach: Reach::Launcher,
        owner: Owner::Elsewhere("tilo-model-manager"),
    },
    Row {
        name: launcher::services::USH_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Elsewhere("tilo-supervisor"),
    },
    Row {
        name: "tilo-fs",
        reach: Reach::Launcher,
        owner: Owner::Elsewhere("the filesystem daemon"),
    },
    Row {
        name: launcher::services::BLOB_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Elsewhere("the filesystem daemon"),
    },
];

/// The services this session started, kept for as long as it runs.
#[derive(Default)]
pub(crate) struct Served {
    /// The identity service apps sign in through: the daemon's, each connection handed to it.
    identity: Option<crate::app_socket::AppSocket>,
    /// The machine's readings, served to the apps the launcher connects.
    system_info: Option<Server>,
    /// What this device's traffic counters said since the last pass.
    usage: Option<data_usage::Counting>,
    /// The typed opens an app asks this session for, and the lists Files reads through it.
    tools: Option<tools::Tools>,
    /// The apps' render services, one per workspace, and their supervision.
    render: Option<render::Broker>,
    /// The Windows compatibility service.
    windows: Option<crate::windows_compat::Served>,
    /// The connections to accounts' servers, made for Settings.
    accounts: Option<accounts::Served>,
    /// The files apps ask the person for, and the links and files they open.
    chooser: Option<chooser::Served>,
    /// The person's clipboard, for the apps that hold no display of their own.
    clipboard: Option<clipboard::Served>,
    /// The notifications the apps here show, in the same centre as the desktop's own notices.
    notify: Option<notify::Served>,
    /// The system's media controls over every Playback the apps here play: what the apps told
    /// the loop since its last pass is taken on the loop itself.
    media: std::cell::RefCell<Option<crate::media_controls::Serving>>,
}

/// What a session serves its services with.
pub(crate) struct Serving {
    /// The seat's shell: which workspaces' apps draw privately, and each workspace's sandbox a
    /// Windows program reaches, read on the loop.
    pub handle: tilo_session_core::desktop::state::DesktopHandle,
    /// Whether the session serves its apps an app wire: one that serves none serves no identity
    /// and no Windows programs either.
    pub serves_apps: bool,
    /// The person at this device: whose workspaces are theirs alone, so their apps share one render
    /// service.
    pub person: String,
    /// The render node the compositor composites on: what a render service draws on, and opens for
    /// its buffers.
    pub render_node: Option<std::path::PathBuf>,
    /// The session program a render service runs as ([`crate::Options::render_service`]): this
    /// process's own when none is named.
    pub render_service: Option<std::path::PathBuf>,
    /// What a capability is asked through, when there is anyone to ask.
    pub questions: Option<AskSender>,
    /// The person's answers, which the portal keeps: an answer given to one question stands for the
    /// other.
    pub consents: Option<tilo_portal::consent::Consents>,
    /// The typed opens an app asks this session for, whose lists only the loop keeps current:
    /// started here, and handed here so the broker can serve Files' waits for its lists' changes.
    pub tools: Option<tools::Handle>,
    /// What wakes the session's loop when a worker read something that the loop counts.
    pub wake: tilo_service::Wake,
    /// What the Windows compatibility service is served with, for a session that starts apps.
    pub windows: Option<crate::windows_compat::Serving>,
    /// The files people choose for apps outside any workspace: the one table the portal grants
    /// through and the chooser answers from.
    pub chosen: Option<Arc<tilo_dialogs::chosen::ChosenTable>>,
    /// The person's workspaces as the device's daemon publishes them: where a chosen file of a
    /// workspace's tree is granted to the app on its node. `None` when this session reaches no
    /// daemon.
    pub view: Option<Arc<crate::workspace_view::WorkspaceView>>,
    /// The shell link the device's daemon handed this session: its identity service is the one
    /// the apps sign in through. `None` when the daemon handed it none, and then no app signs in.
    pub daemon: Option<runtime_collab::ShellClient>,
    /// The notifications the session shows: what an app sends over its connection reaches the
    /// same daemon as the desktop's own notices. `None` when the session serves no notifications.
    pub notifications: Option<tilo_notifications_service::Notifications>,
}

/// Start every service of [`TABLE`] this module owns, and keep what serves them for as long as the
/// session runs.
pub(crate) fn start(mut serving: Serving) -> Served {
    let mut served = Served {
        usage: data_usage::Counting::start(serving.wake.clone()),
        ..Served::default()
    };
    for row in TABLE {
        // What this session does and does not serve, as its log reads it: the same table the guard
        // below holds every client to.
        tracing::debug!(
            service = row.name,
            reach = ?row.reach,
            owner = ?row.owner,
            "a service of the session"
        );
        let Owner::Session(service) = row.owner else {
            continue;
        };
        match service {
            Service::Identity => {
                served.identity = serving
                    .daemon
                    .clone()
                    .and_then(|daemon| identity_at_daemon(serving.serves_apps, daemon));
            }
            Service::Render => {
                match render::Broker::start(
                    &serving.handle,
                    serving.person.clone(),
                    serving.render_node.take(),
                    serving.render_service.take(),
                ) {
                    Ok(broker) => served.render = Some(broker),
                    Err(error) => tracing::warn!(%error, "the apps' render broker is unavailable"),
                }
            }
            Service::SystemInfo => {
                served.system_info =
                    system_info(serving.questions.clone(), serving.consents.take());
            }
            Service::Tools => {
                served.tools =
                    serving
                        .tools
                        .take()
                        .and_then(|handle| match tools::Tools::serve(handle) {
                            Ok(tools) => Some(tools),
                            Err(error) => {
                                tracing::warn!(%error, "the tools service is unavailable");
                                None
                            }
                        });
            }
            Service::Windows => {
                served.windows = windows(&mut serving);
            }
            Service::Accounts => {
                served.accounts = accounts::serve(serving.wake.clone())
                    .inspect_err(
                        |error| tracing::warn!(%error, "the accounts service is unavailable"),
                    )
                    .ok();
            }
            Service::Chooser => {
                served.chooser = chooser::serve(chooser::Serving {
                    questions: serving.questions.clone(),
                    chosen: serving
                        .chosen
                        .take()
                        .unwrap_or_else(tilo_dialogs::chosen::chosen_table),
                    files: serving.view.as_ref().map(|view| {
                        Arc::new(chooser::PolicyFiles::new(
                            Arc::clone(view),
                            serving.person.clone(),
                        )) as Arc<dyn chooser::WorkspaceFiles>
                    }),
                    wake: serving.wake.clone(),
                })
                .inspect_err(|error| tracing::warn!(%error, "the chooser service is unavailable"))
                .ok();
            }
            Service::Clipboard => {
                served.clipboard = clipboard::serve(serving.wake.clone())
                    .inspect_err(|error| {
                        tracing::warn!(%error, "the clipboard service is unavailable");
                    })
                    .ok();
            }
            Service::Notify => {
                served.notify = serving.notifications.take().and_then(|notifications| {
                    match notify::serve(notifications) {
                        Ok(served) => Some(served),
                        Err(error) => {
                            tracing::warn!(%error, "the notification service is unavailable");
                            None
                        }
                    }
                });
            }
            Service::DataUsage => {}
        }
    }
    served
}

impl Served {
    /// The typed opens an app asks this session for, while it serves them.
    pub(crate) fn tools(&self) -> Option<&tools::Handle> {
        self.tools.as_ref().map(tools::Tools::handle)
    }

    /// The sign-in pages Settings' account sign-ins ask this session for, while it serves them.
    pub(crate) const fn accounts(&self) -> Option<&accounts::Served> {
        self.accounts.as_ref()
    }

    /// The files and links the apps ask this session for, while it serves them.
    pub(crate) const fn chooser(&self) -> Option<&chooser::Served> {
        self.chooser.as_ref()
    }

    /// Serve the media controls over the Playbacks the apps here play.
    pub(crate) fn serve_media(&self, media: crate::media_controls::Serving) {
        *self.media.borrow_mut() = Some(media);
    }

    /// Take what the apps said they play since the last pass, and show it.
    pub(crate) fn follow_media(&self) {
        if let Some(media) = &mut *self.media.borrow_mut() {
            media.follow();
        }
    }

    /// The person's clipboard, served to the tilolang apps that hold no display of their own.
    pub(crate) const fn clipboard(&self) -> Option<&clipboard::Served> {
        self.clipboard.as_ref()
    }

    /// Count what this device's traffic did since the last pass into the person's settings, and
    /// bring what the services read of the seat's shell `handle` and the daemon's `view` level
    /// with them, on the loop's pass: the only thread that may touch the services or the shell.
    pub(crate) fn follow(
        &self,
        services: &mut Services,
        handle: &tilo_session_core::desktop::state::DesktopHandle,
        view: Option<&crate::workspace_view::WorkspaceView>,
    ) {
        if let Some(usage) = &self.usage {
            usage.count(services);
        }
        if let Some(render) = &self.render {
            render.sync_workspaces(handle);
        }
        if let Some(windows) = &self.windows {
            windows.answer_sandboxes(handle, view);
        }
    }
}

/// The Windows compatibility service at the launcher's socket for it, for a session whose apps
/// start through a launcher and that serves them an app wire.
fn windows(serving: &mut Serving) -> Option<crate::windows_compat::Served> {
    if !serving.serves_apps {
        return None;
    }
    crate::windows_compat::serve(serving.windows.take()?, serving.wake.clone())
        .inspect_err(
            |error| tracing::warn!(%error, "the Windows compatibility service is unavailable"),
        )
        .ok()
}

/// The device daemon's identity service at the launcher's socket for it, each connection handed
/// to the daemon over its shell link `daemon`: the daemon signs in the workspace an app names, as
/// the one identity service of the device. A session that serves no app wire (`serves`) has
/// nothing to sign in.
fn identity_at_daemon(
    serves: bool,
    daemon: runtime_collab::ShellClient,
) -> Option<crate::app_socket::AppSocket> {
    if !serves {
        return None;
    }
    if let Err(error) = launcher::services::ensure_services_root() {
        tracing::warn!(%error, "the identity service is unavailable");
        return None;
    }
    let path = launcher::services::socket_for(launcher::services::IDENTITY_SERVICE);
    match crate::app_socket::serve_identity(&path, daemon) {
        Ok(socket) => {
            tracing::info!("serving the device's app identity to the apps the launcher starts");
            Some(socket)
        }
        Err(error) => {
            tracing::warn!(%error, "the identity service is unavailable");
            None
        }
    }
}

/// The machine's readings at the launcher's socket for them, from this session's one hub: what the
/// shell's monitor shows and what an app reads are the same readings, and a grant the app does not
/// carry is asked of the person as any other capability is.
fn system_info(
    questions: Option<AskSender>,
    consents: Option<tilo_portal::consent::Consents>,
) -> Option<Server> {
    if let Err(error) = launcher::services::ensure_services_root() {
        tracing::warn!(%error, "the system information service is unavailable");
        return None;
    }
    let path = launcher::services::socket_for(tilo_system_stats::SERVICE);
    let prompter: Arc<dyn Prompter> = match consents.or_else(|| {
        questions.map(|questions| {
            tilo_portal::consent::Consents::new(questions, tilo_peer_state::DeviceGrants::unkept())
        })
    }) {
        Some(consents) => Arc::new(ConsentsPrompter { consents }),
        // A session with nobody to ask still serves the readings: the apps the image hands the
        // machine to read them, and anyone else is answered no.
        None => Arc::new(tilo_system_stats::server::NoPrompter),
    };
    match Server::bind(&path, Hub::shared().clone(), prompter) {
        Ok(server) => {
            tracing::info!("serving the machine's readings to the apps the launcher starts");
            Some(server)
        }
        Err(error) => {
            tracing::warn!(
                %error,
                path = %path.display(),
                "the system information service is unavailable"
            );
            None
        }
    }
}

/// The person's answers, as the system information service asks them: the same decisions the portal
/// keeps, so an allowance given to one question stands for the other.
struct ConsentsPrompter {
    consents: tilo_portal::consent::Consents,
}

/// The device of the machine a reading of the system information service is.
const fn device(grant: Grant) -> DeviceCap {
    match grant {
        Grant::SystemInfo => DeviceCap::SystemInfo,
        Grant::Processes => DeviceCap::Processes,
        Grant::CommandLines => DeviceCap::ProcessCommands,
    }
}

/// Who asks: the app, in the workspace its launcher named.
fn caller(handoff: &Handoff) -> PortalCaller {
    PortalCaller {
        workspace: handoff.workspace_id.clone(),
        ..PortalCaller::new(handoff.app_id.clone())
    }
}

impl Prompter for ConsentsPrompter {
    fn granted(&self, handoff: &Handoff, grant: Grant) -> bool {
        self.consents.allowed(&caller(handoff), device(grant))
    }

    fn ask(&self, handoff: &Handoff, grant: Grant) -> std::sync::mpsc::Receiver<bool> {
        let caller = caller(handoff);
        let device = device(grant);
        let question = tilo_portal::consent::AccessQuestion {
            capability: device.name().to_owned(),
            reason: None,
            may_remember: true,
            text: None,
        };
        let handle = format!("system-stats:{}:{}", caller.app_id, question.capability);
        // The connection's own thread waits for the answer; the service and the session go on,
        // and the sheet is shown on the loop's next pass.
        let (answer, answered) = std::sync::mpsc::channel();
        let consents = self.consents.clone();
        drop(crate::landing::off_loop(async move {
            let _ = answer.send(
                consents
                    .decide(&caller, &handle, device, question)
                    .await
                    .is_ok(),
            );
        }));
        answered
    }
}

/// This device's data usage, counted where the device is read: the session
/// reads the wired and wireless interfaces' and the modems' traffic totals
/// and counts them into the person's `network-usage` section through its
/// `count-data-used` verb, whatever window is open. So the count is a fact
/// of the device that agents, scripts and Settings read alike, not
/// something a page keeps up while it is shown.
///
/// The totals are read every minute but counted only every quarter hour,
/// and at once where a counter moves to another connection, stops, or a new
/// local day begins: then the reading before the change is counted first,
/// so traffic lands on the connection and the day it moved on.
mod data_usage {
    use super::{ConnectionKind, Duration, Map, Reading, Services, Value, mpsc};

    /// How often the counters are read while this device has a connection up.
    const READ_EVERY: Duration = Duration::from_secs(60);
    /// How often a device with nothing to count looks again.
    const IDLE: Duration = Duration::from_secs(600);
    /// How long a reading may wait to be counted while nothing changes.
    const COUNT_EVERY: i64 = 15 * 60;

    /// The section's kind, and its verb that counts traffic totals.
    const SECTION: &str = "settings.network-usage";
    const COUNT: &str = "count-data-used";

    /// A reading taken at one moment, to be counted.
    struct Due {
        readings: Vec<Reading>,
        at: i64,
        offset: i32,
    }

    /// The worker reading this device's counters, and what it read since the loop's last pass.
    pub(super) struct Counting {
        due: mpsc::Receiver<Due>,
        boot: Option<String>,
        _worker: std::thread::JoinHandle<()>,
    }

    impl Counting {
        /// Start reading this device's counters, waking the loop with `wake` for each reading it
        /// counts. The worker ends with what it reads into.
        pub(super) fn start(wake: tilo_service::Wake) -> Option<Self> {
            let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .ok()
                .map(|id| id.trim().to_owned());
            let (sender, due) = mpsc::channel();
            let worker = std::thread::Builder::new()
                .name("tilo-network-usage".into())
                .spawn(move || {
                    let mut counter = Counter::default();
                    loop {
                        let at = tilo_clock::unix_now_i64();
                        let offset = jiff::Zoned::now().offset().seconds();
                        let readings = read();
                        let idle = readings.is_empty();
                        for due in counter.take(Taken {
                            readings,
                            at,
                            offset,
                        }) {
                            if sender.send(due).is_err() {
                                return;
                            }
                            wake();
                        }
                        std::thread::park_timeout(if idle { IDLE } else { READ_EVERY });
                    }
                })
                .map_err(|error| tracing::warn!(%error, "the data used is not counted"))
                .ok()?;
            Some(Self {
                due,
                boot,
                _worker: worker,
            })
        }

        /// Count what the worker read since the last pass.
        pub(super) fn count(&self, services: &mut Services) {
            while let Ok(due) = self.due.try_recv() {
                let arguments = arguments(&due.readings, self.boot.as_deref(), due.at, due.offset);
                let counted = services.invoke(
                    &settings_sections::address(SECTION).unwrap_or_default(),
                    COUNT,
                    &arguments,
                );
                match counted {
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        tracing::warn!(%error, "the data used was not counted");
                    }
                    None => {
                        tracing::warn!(
                            "the data used was not counted: no settings service answers"
                        );
                    }
                }
            }
        }
    }

    /// The counters as one moment read them.
    struct Taken {
        readings: Vec<Reading>,
        at: i64,
        offset: i32,
    }

    impl Taken {
        fn day(&self) -> Option<jiff::civil::Date> {
            settings_sections::network_usage::local_day(self.at, self.offset)
        }

        /// Whether `next` counts on another connection or day than this: a counter on another
        /// connection or gone, or another local day.
        fn changes_at(&self, next: &Self) -> bool {
            self.day() != next.day()
                || self.readings.iter().any(|before| {
                    !next.readings.iter().any(|after| {
                        after.counter == before.counter && after.connection == before.connection
                    })
                })
        }
    }

    /// What was read and what was counted, between readings.
    #[derive(Default)]
    struct Counter {
        /// The last reading, while it is not counted yet.
        held: Option<Taken>,
        /// When a reading was last counted.
        counted_at: Option<i64>,
    }

    impl Counter {
        /// Take in a reading; the readings to count now, in order, each with when it was taken and
        /// the local offset then.
        fn take(&mut self, now: Taken) -> Vec<Due> {
            let mut count = Vec::new();
            let changed = self.held.take().filter(|held| held.changes_at(&now));
            if let Some(before) = changed {
                count.push(Due {
                    readings: before.readings,
                    at: before.at,
                    offset: before.offset,
                });
            }
            // What is on the counters when this session starts was used before it asked, and the
            // count that follows carries it forward: only a change, or a quarter hour of counting,
            // is due.
            let due = !count.is_empty()
                || self
                    .counted_at
                    .is_some_and(|at| now.at.saturating_sub(at) >= COUNT_EVERY);
            if due && !now.readings.is_empty() {
                self.counted_at = Some(now.at);
                count.push(Due {
                    readings: now.readings,
                    at: now.at,
                    offset: now.offset,
                });
            } else {
                self.counted_at.get_or_insert(now.at);
                self.held = Some(now);
            }
            count
        }
    }

    /// Every counter there is to read now.
    fn read() -> Vec<Reading> {
        let mut readings: Vec<Reading> = os_actions::read_interface_traffic()
            .unwrap_or_default()
            .into_iter()
            .map(|interface| {
                let (kind, prefix) = if interface.wired {
                    (ConnectionKind::Wired, "wired")
                } else {
                    (ConnectionKind::Wifi, "wifi")
                };
                reading(
                    interface.interface,
                    format!("{prefix}:{}", interface.uuid),
                    kind,
                    interface.name,
                    interface.traffic,
                )
            })
            .collect();
        let modems = os_actions::read_mobile()
            .map(|reading| reading.modems)
            .unwrap_or_default();
        readings.extend(modems.into_iter().filter_map(|modem| {
            let traffic = modem.traffic?;
            let sim = modem.active_sim();
            let id = sim
                .and_then(|sim| sim.iccid.clone())
                .unwrap_or_else(|| modem.path.clone());
            let name = sim
                .map(os_actions::modem::SimCard::label)
                .or_else(|| modem.operator.clone())
                .unwrap_or_else(|| modem.name.clone());
            Some(reading(
                format!("modem:{}", modem.path),
                format!("mobile:{id}"),
                ConnectionKind::Mobile,
                name,
                traffic,
            ))
        }));
        readings
    }

    fn reading(
        counter: String,
        connection: String,
        kind: ConnectionKind,
        name: String,
        traffic: os_actions::modem::Traffic,
    ) -> Reading {
        Reading {
            counter,
            connection,
            kind,
            name,
            received: traffic.received,
            sent: traffic.sent,
        }
    }

    /// The arguments of `count-data-used` for `readings` .
    fn arguments(
        readings: &[Reading],
        boot: Option<&str>,
        at: i64,
        offset: i32,
    ) -> Map<String, Value> {
        let mut arguments = Map::new();
        arguments.insert(
            "readings".into(),
            serde_json::to_value(readings).unwrap_or_default(),
        );
        arguments.insert("boot".into(), boot.into());
        arguments.insert("at".into(), at.into());
        arguments.insert("offset".into(), offset.into());
        arguments
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn on(connection: &str, received: u64) -> Reading {
            reading(
                "wlan0".into(),
                connection.into(),
                ConnectionKind::Wifi,
                connection.into(),
                os_actions::modem::Traffic { received, sent: 0 },
            )
        }

        fn taken(readings: Vec<Reading>, at: i64) -> Taken {
            Taken {
                readings,
                at,
                offset: 0,
            }
        }

        /// 2026-03-14 12:00 UTC.
        const NOON: i64 = 1_773_489_600;

        tilo_test_env::scenario! {
            fn a_connections_readings_are_counted_and_sent_as_the_verbs_arguments;

            phase readings_are_counted_on_their_own_connection;
            // Readings wait a quarter hour to be counted, but one before a change of connection is
            // counted at once, ahead of the one after it. The first reading is where the session
            // starts counting from.
            {
                let mut counter = Counter::default();
                assert!(
                    counter.take(taken(vec![on("wifi:a", 1)], NOON)).is_empty(),
                    "the first reading is the baseline"
                );
                assert!(
                    counter
                        .take(taken(vec![on("wifi:a", 2)], NOON + 60))
                        .is_empty()
                );
                let moved = counter.take(taken(vec![on("wifi:b", 4)], NOON + 120));
                let order: Vec<(&str, u64)> = moved
                    .iter()
                    .map(|due| {
                        (
                            due.readings[0].connection.as_str(),
                            due.readings[0].received,
                        )
                    })
                    .collect();
                assert_eq!(order, [("wifi:a", 2), ("wifi:b", 4)]);
                assert!(
                    counter
                        .take(taken(vec![on("wifi:b", 5)], NOON + 180))
                        .is_empty()
                );
                assert_eq!(
                    counter
                        .take(taken(vec![on("wifi:b", 6)], NOON + 180 + COUNT_EVERY))
                        .len(),
                    1,
                    "a quarter hour on"
                );
                assert!(counter.take(taken(Vec::new(), NOON + 240)).is_empty());
            }

            phase the_readings_are_the_verbs_arguments;
            {
                let arguments = arguments(&[on("wifi:a", 1_000)], Some("b"), 10, 3_600);
                assert_eq!(arguments["readings"][0]["connection"], "wifi:a");
                assert_eq!(arguments["readings"][0]["kind"], "wifi");
                assert_eq!(arguments["readings"][0]["received"], 1_000);
                assert_eq!(arguments["boot"], "b");
                assert_eq!(arguments["offset"], 3_600);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{Owner, Reach, Service, TABLE};

    /// The names a client in this tree dials: what the launcher hands an app at its launch
    /// ([`launcher::services::HANDED_SERVICES`]), what every app's manifest declares, and what a
    /// tilolang package's generated manifest and its runtime ask for.
    fn dialled_by_clients() -> BTreeSet<String> {
        let mut dialled: BTreeSet<String> = launcher::services::HANDED_SERVICES
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        dialled.extend(declared());
        dialled
    }

    /// The names the apps dial: every service a native app's manifest declares
    /// under `[sandbox.services]`, and the two a tilolang package's generated
    /// manifest names.
    fn declared() -> Vec<String> {
        let apps = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
        let mut manifests = std::fs::read_dir(&apps)
            .expect("the workspace's apps are readable")
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("tilo-app.toml"))
            .filter(|manifest| manifest.is_file())
            .collect::<Vec<_>>();
        manifests.sort();
        let (native, tilolang) = default_collection(&apps);
        assert_eq!(
            manifests.len(),
            native.len(),
            "the session read {} app manifests, but the default collection packages {}",
            manifests.len(),
            native.len()
        );
        let mut names = Vec::new();
        for manifest in manifests {
            let source = std::fs::read_to_string(&manifest).expect("an app manifest is readable");
            names.extend(manifest_services(&source));
        }
        // A tilolang package has no manifest in the tree: `tilolang build`
        // generates it, naming collaboration and the filesystem at the
        // runtime's version (crates/lang/tilolang-cli/src/package.rs), and the
        // session hands every instance the tools service the runtime asks it
        // for (runtime-portal's device processes).
        for _ in tilolang {
            names.push("collab".to_owned());
            names.push("tilo-fs".to_owned());
            names.push(crate::tools::SERVICE.to_owned());
        }
        names
    }

    /// The default collection's apps, from `dist/apps/app-collections.toml` as
    /// `package-apps` and the image read it: `*` is every native app's manifest
    /// under `apps/`, and each `tl-<name>` is the tilolang package in
    /// `apps/tl/<name>` that replaced the native app it retired. The list is the
    /// source, never a count: every app it names must be in the tree.
    fn default_collection(
        apps: &std::path::Path,
    ) -> (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) {
        let repo = apps.parent().expect("apps/ is under the repository root");
        let config = std::fs::read_to_string(repo.join("dist/apps/app-collections.toml"))
            .expect("the app collections read");
        let include = config
            .lines()
            .skip_while(|line| line.trim() != "[collections.default]")
            .skip(1)
            .find_map(|line| line.trim().strip_prefix("include = "))
            .expect("dist/apps/app-collections.toml names a default include");
        let entries: Vec<&str> = include
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .map(|entry| entry.trim().trim_matches('"'))
            .filter(|entry| !entry.is_empty())
            .collect();
        let mut native = Vec::new();
        let mut tilolang = Vec::new();
        for entry in entries {
            if entry == "*" {
                let mut manifests: Vec<std::path::PathBuf> = std::fs::read_dir(apps)
                    .unwrap_or_else(|e| panic!("read {}: {e}", apps.display()))
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|dir| dir.join("tilo-app.toml").is_file())
                    .collect();
                manifests.sort();
                assert!(!manifests.is_empty(), "`*` packs no app under apps/");
                native.extend(manifests);
                continue;
            }
            if let Some(name) = entry.strip_prefix("tl-") {
                let package = apps.join("tl").join(name);
                assert!(
                    package.join("app.tl").is_file(),
                    "{entry} is packaged but {} is missing",
                    package.join("app.tl").display()
                );
                tilolang.push(package);
                continue;
            }
            let manifest = apps.join(entry).join("tilo-app.toml");
            assert!(
                manifest.is_file(),
                "{entry} is packaged but {} is missing",
                manifest.display()
            );
            native.push(apps.join(entry));
        }
        native.sort();
        native.dedup();
        (native, tilolang)
    }

    /// The names a manifest declares under `[sandbox.services]` .
    fn manifest_services(source: &str) -> Vec<String> {
        let mut names = Vec::new();
        let mut inside = false;
        for line in source.lines() {
            let line = line.trim();
            if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                inside = section == "sandbox.services" || section.starts_with("sandbox.services.");
                if let Some(name) = section.strip_prefix("sandbox.services.") {
                    names.push(name.to_owned());
                }
                continue;
            }
            if inside
                && let Some((name, _)) = line.split_once('=')
                && !name.trim().is_empty()
                && name
                    .trim()
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                names.push(name.trim().to_owned());
            }
        }
        names
    }

    tilo_test_env::scenario! {
        fn the_table_of_services_has_one_row_for_everything_dialled_or_started;

        phase every_service_a_client_dials_has_a_row;
        // A service a client dials is never silently absent from this session: every name the launcher
        // hands an app or an app declares has a row saying who serves it, or why nothing does. A
        // service this module owns is started by `start` , whose match over `Service` is exhaustive.
        {
            let missing: Vec<String> = dialled_by_clients()
                .into_iter()
                .filter(|name| !TABLE.iter().any(|row| row.name == name))
                .collect();
            assert!(
                missing.is_empty(),
                "no row serves {missing:?}: add one to the table in services/mod.rs"
            );
        }

        phase the_table_names_each_service_once;
        // Two rows for one name would be two servers on one socket: the table names each service once.
        {
            let mut seen = BTreeSet::new();
            for row in TABLE {
                assert!(
                    seen.insert(row.name),
                    "`{}` has more than one row",
                    row.name
                );
            }
        }

        phase every_row_says_how_it_is_reached_and_why_not;
        // A row this module starts reaches its clients, a row served elsewhere names where, and a row
        // nothing serves says why.
        {
            for row in TABLE {
                match row.owner {
                    Owner::Session(service) => assert_ne!(
                        row.reach,
                        Reach::NoConnection,
                        "`{}` is started but reached nowhere ({service:?})",
                        row.name
                    ),
                    Owner::Module(module) => assert!(
                        !module.is_empty() && row.reach != Reach::AppWire,
                        "`{}` names the module `{module}` but is reached on the app wire",
                        row.name
                    ),
                    Owner::Elsewhere(program) => assert!(
                        !program.is_empty() && row.reach == Reach::Launcher,
                        "`{}` names `{program}` as its server on no socket",
                        row.name
                    ),
                }
            }
        }

        phase every_started_service_has_a_row;
        // Every service [`super::start`] starts has a row, so the table is what a client's reader
        // sees, and nothing is started outside it.
        {
            for service in [
                Service::Identity,
                Service::SystemInfo,
                Service::DataUsage,
                Service::Tools,
                Service::Render,
                Service::Windows,
                Service::Accounts,
                Service::Chooser,
                Service::Clipboard,
                Service::Notify,
            ] {
                assert!(
                    TABLE
                        .iter()
                        .any(|row| row.owner == Owner::Session(service)),
                    "{service:?} has no row"
                );
            }
        }

        phase the_dialled_names_come_from_the_code_that_dials_them;
        // The derivation reads code, so a source or manifest it cannot read fails here rather than
        // passing an empty set.
        {
            let names = dialled_by_clients();
            for known in [
                launcher::services::COLLAB_SERVICE,
                launcher::services::IDENTITY_SERVICE,
                launcher::services::CHOOSER_SERVICE,
                launcher::services::COMPAT_SERVICE,
                tilo_system_stats::SERVICE,
                tilo_render_protocol::broker::SERVICE,
                crate::tools::SERVICE,
                launcher::services::MESSAGING_SERVICE,
                "tilo-fs",
            ] {
                assert!(
                    names.contains(known),
                    "`{known}` is dialled by the launcher or an app's manifest, and the derivation \
                 missed it"
                );
            }
            assert_eq!(
                manifest_services(
                    "[sandbox.services]\ncollab = \"optional@16\"\n\n[entrypoint]\nbin = \"x\"\n"
                ),
                ["collab"],
                "an app's declared services are read"
            );
        }
    }
}
