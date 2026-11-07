//! Every service this session serves, in one table: the name a client
//! dials it by, how the client reaches it, and what serves it.
//!
//! The session's apps and the person's own devices reach it in two ways. A
//! socket under the launcher's services root is what the launcher hands an
//! app at launch and what a program of this login dials by name
//! ([`launcher::services::socket_for`]): one name, one socket, one server.
//! Everything else is a registration: a resource the apps reach on the
//! action plane they already speak on, or, for the work no one calls at
//! all, a worker that counts into the person's own records.
//!
//! [`TABLE`] is the whole answer to "does this session serve X". The
//! services this module starts start from [`start`], which walks the table,
//! and the match over [`Service`] is exhaustive: a row this module owns
//! without code to start it does not compile. The rows served by a module
//! beside this one name that module, and the rows another program of this
//! login serves name the program; a row nothing serves carries the reason,
//! so a gap is read here rather than found by an app whose service is
//! missing.
//!
//! The test below derives what clients dial from the code that dials it —
//! the launcher's own `socket_for` sites and the services every app's
//! manifest declares — and fails when one has no row here.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Map, Value};
use settings_sections::network_usage::{ConnectionKind, Reading};
use tilo_action_broker::channel_encryption::ChannelKeyProvider;
use tilo_identity_host::broker::{
    ChannelKeySource, IdentityBrokerHandle, PersonRecordKinds, WorkspaceMembership,
};
use tilo_portal::PortalCaller;
use tilo_service::{Questions, Services, Started};
use tilo_system_stats::Grant;
use tilo_system_stats::hub::Hub;
use tilo_system_stats::protocol::Handoff;
use tilo_system_stats::server::{Prompter, Server};

use crate::plane::DeskAuthority;

use super::render;
use super::tools;

/// How a client reaches a service of this session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reach {
    /// On the launcher's socket for it, under the services root
    /// ([`launcher::services::socket_for`]): what it hands an app at launch,
    /// and what a program of this login dials by name.
    Launcher,
    /// On the action plane the session's apps speak on: a resource
    /// published beside the others, or a verb of the person's own records.
    Plane,
    /// Nowhere: a package names the service in its manifest for the grant
    /// it carries, and there is no connection.
    NoConnection,
}

/// What serves a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    /// This module, from [`start`].
    Session(Service),
    /// The module of the session beside this one, named as the reader finds
    /// it.
    Module(&'static str),
    /// Another program of this login.
    Elsewhere(&'static str),
    /// Nothing on this desktop yet, and why.
    Nothing(&'static str),
}

/// The services [`start`] starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Service {
    /// The identity broker apps sign in through.
    Identity,
    /// The machine's readings, for the apps the launcher connects and the
    /// person grants.
    SystemInfo,
    /// This device's data usage, counted into the person's settings.
    DataUsage,
    /// The typed opens an app asks this session for: a tool at a folder, the
    /// person's drives and apps, a workspace settings tab.
    Tools,
    /// The apps' render services: a toolkit app hands its frame to the
    /// service of its workspace, which draws it on the GPU.
    Render,
}

/// One service of this desktop: its name, how it is reached, and what
/// serves it.
pub(crate) struct Door {
    pub name: &'static str,
    pub reach: Reach,
    pub owner: Owner,
}

/// Every service this session's apps and devices reach.
pub(crate) const TABLE: &[Door] = &[
    Door {
        name: launcher::services::COLLAB_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Module("mount::services"),
    },
    Door {
        name: launcher::services::SHELL_UI_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Module("mount::app_tabs"),
    },
    Door {
        name: launcher::services::IDENTITY_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Session(Service::Identity),
    },
    Door {
        name: tilo_system_stats::SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Session(Service::SystemInfo),
    },
    Door {
        name: launcher::services::SYSTEM_SERVICE,
        reach: Reach::NoConnection,
        owner: Owner::Module("mount::services"),
    },
    Door {
        name: "network-usage",
        reach: Reach::Plane,
        owner: Owner::Session(Service::DataUsage),
    },
    Door {
        name: launcher::services::CHOOSER_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Nothing(
            "the identity and application choosers an app asks the shell for on this socket are \
             not served by this session yet",
        ),
    },
    Door {
        name: launcher::services::COMPAT_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Nothing(
            "a Windows application's handoff reaches no compatibility service in this session \
             yet, because no session runs the appliance it would start",
        ),
    },
    Door {
        name: tools::SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Session(Service::Tools),
    },
    Door {
        name: tilo_render_protocol::broker::SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Session(Service::Render),
    },
    Door {
        name: launcher::services::MESSAGING_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Elsewhere("tilo-messaging"),
    },
    Door {
        name: launcher::services::MODEL_GATEWAY_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Elsewhere("tilo-model-gateway"),
    },
    Door {
        name: "models",
        reach: Reach::Launcher,
        owner: Owner::Elsewhere("tilo-model-manager"),
    },
    Door {
        name: launcher::services::APP_HOST_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Elsewhere("tilo-app-host"),
    },
    Door {
        name: launcher::services::USH_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Elsewhere("tilo-supervisor"),
    },
    Door {
        name: "tilo-fs",
        reach: Reach::Launcher,
        owner: Owner::Elsewhere("the filesystem daemon"),
    },
    Door {
        name: launcher::services::BLOB_SERVICE,
        reach: Reach::Launcher,
        owner: Owner::Elsewhere("the filesystem daemon"),
    },
];

/// The services this session started, kept for as long as it runs.
#[derive(Default)]
pub(crate) struct Doors {
    /// The identity broker apps sign in through.
    identity: Option<IdentityBrokerHandle>,
    /// The machine's readings, served to the apps the launcher connects.
    system_info: Option<Server>,
    /// What this device's traffic counters said since the last pass.
    usage: Option<data_usage::Counting>,
    /// The typed opens an app asks this session for, and the lists Files
    /// reads through it.
    tools: Option<tools::Tools>,
    /// The apps' render services, one per workspace, and their supervision.
    render: Option<render::Broker>,
}

/// What a session serves its services with.
pub(crate) struct Serving {
    /// The desk the session's apps act on, and the workspace their identity
    /// is signed in: a session that serves no action plane serves no
    /// identity either. Its apps still render, all in the private service.
    pub desk: Option<Arc<DeskAuthority>>,
    /// The person at this device: whose workspaces are theirs alone, so
    /// their apps share one render service.
    pub person: String,
    /// The render node the compositor composites on: what a render service
    /// draws on, and opens for its buffers.
    pub render_node: Option<std::path::PathBuf>,
    /// Where the identity service finds this device's channel keys.
    pub keys: ChannelKeyProvider,
    /// What a capability is asked through, when there is anyone to ask.
    pub questions: Option<Questions>,
    /// The person's answers, which the portal keeps: an answer given to one
    /// question stands for the other.
    pub consents: Option<tilo_portal::consent::Consents>,
    /// The typed opens an app asks this session for, whose lists only the
    /// loop keeps current: started here, and handed here so the broker can
    /// serve Files' waits for its lists' changes.
    pub tools: Option<tools::Handle>,
    /// What wakes the session's loop when a worker read something that the
    /// loop counts.
    pub wake: tilo_service::Wake,
}

/// Start every service of [`TABLE`] this module owns, and keep what serves
/// them for as long as the session runs.
pub(crate) fn start(mut serving: Serving) -> Doors {
    let mut doors = Doors {
        usage: data_usage::Counting::start(serving.wake.clone()),
        ..Doors::default()
    };
    for door in TABLE {
        // What this session does and does not serve, as its log reads it:
        // the same table the guard below holds every client to.
        tracing::debug!(
            service = door.name,
            reach = ?door.reach,
            owner = ?door.owner,
            "a service of the session"
        );
        let Owner::Session(service) = door.owner else {
            continue;
        };
        match service {
            Service::Identity => {
                doors.identity = identity(
                    serving.desk.as_ref().map(Arc::clone),
                    std::mem::take(&mut serving.keys),
                );
            }
            Service::Render => {
                let desk = serving.desk.as_ref().map(Arc::clone);
                match render::Broker::start(
                    desk,
                    serving.person.clone(),
                    serving.render_node.take(),
                ) {
                    Ok(broker) => doors.render = Some(broker),
                    Err(error) => tracing::warn!(%error, "the apps' render broker is unavailable"),
                }
            }
            Service::SystemInfo => {
                doors.system_info = system_info(serving.questions.take(), serving.consents.take());
            }
            Service::Tools => {
                doors.tools =
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
            Service::DataUsage => {}
        }
    }
    doors
}

impl Doors {
    /// The typed opens an app asks this session for, while it serves them.
    pub(crate) fn tools(&self) -> Option<&tools::Handle> {
        self.tools.as_ref().map(tools::Tools::handle)
    }

    /// Count what this device's traffic did since the last pass into the
    /// person's settings, on the loop's pass: the only thread that may
    /// touch the services.
    pub(crate) fn follow(&self, services: &mut Services) {
        if let Some(usage) = &self.usage {
            usage.count(services);
        }
        if let Some(render) = &self.render {
            render.sync_workspaces();
        }
    }
}

/// The identity service at the launcher's socket for it, signing in the
/// workspace the desk shows.
fn identity(
    desk: Option<Arc<DeskAuthority>>,
    keys: ChannelKeyProvider,
) -> Option<IdentityBrokerHandle> {
    // An app reaches its identity the way it reaches its actions: over the
    // plane this session serves it. A session that serves none has nothing
    // to sign in.
    let desk = desk?;
    if let Err(error) = launcher::services::ensure_services_root() {
        tracing::warn!(%error, "the identity service is unavailable");
        return None;
    }
    let path = launcher::services::socket_for(launcher::services::IDENTITY_SERVICE);
    let membership = desk as Arc<dyn WorkspaceMembership>;
    let keys = Arc::new(keys) as Arc<dyn ChannelKeySource>;
    match tilo_identity_host::broker::spawn(path, membership, keys, Arc::new(InstalledApps::read()))
    {
        Ok(handle) => {
            tracing::info!("serving app identity to the apps the launcher starts");
            Some(handle)
        }
        Err(error) => {
            tracing::warn!(%error, "the identity service is unavailable");
            None
        }
    }
}

/// What the identity service knows of the apps it signs for: the person
/// records their surfaces declare, and the ids each had before as the
/// installed catalogue believes them.
struct InstalledApps {
    earlier: tilo_action_policy::EarlierIds,
}

impl InstalledApps {
    /// The catalogue as this login holds it; an unreadable one leaves the
    /// apps with the ids they have now.
    fn read() -> Self {
        let earlier = match app_catalogue::apps::earlier_ids_from_env() {
            Ok(earlier) => earlier,
            Err(error) => {
                tracing::warn!(%error, "the installed apps are not read: no earlier app ids");
                tilo_action_policy::EarlierIds::default()
            }
        };
        Self { earlier }
    }
}

impl PersonRecordKinds for InstalledApps {
    fn person_record_kinds(&self, app_id: &str) -> Vec<String> {
        tilo_action_broker::SurfacePersonRecords.person_record_kinds(app_id)
    }

    fn earlier_ids(&self, app_id: &str) -> Vec<String> {
        self.earlier.of(app_id).to_vec()
    }
}

/// The machine's readings at the launcher's socket for them, from this
/// session's one hub: what the desk's monitor shows and what an app reads
/// are the same readings, and a grant the app does not carry is asked of
/// the person as any other capability is.
fn system_info(
    questions: Option<Questions>,
    consents: Option<tilo_portal::consent::Consents>,
) -> Option<Server> {
    if let Err(error) = launcher::services::ensure_services_root() {
        tracing::warn!(%error, "the system information service is unavailable");
        return None;
    }
    let path = launcher::services::socket_for(tilo_system_stats::SERVICE);
    let prompter: Arc<dyn Prompter> =
        match consents.or_else(|| questions.map(tilo_portal::consent::Consents::for_session)) {
            Some(consents) => Arc::new(ConsentsPrompter { consents }),
            // A session with nobody to ask still serves the readings: the
            // apps the image hands the machine to read them, and anyone
            // else is answered no.
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

/// The person's answers, as the system information service asks them: the
/// same decisions the portal keeps, so an allowance given to one question
/// stands for the other.
struct ConsentsPrompter {
    consents: tilo_portal::consent::Consents,
}

/// Who asks: the app, in the workspace its launcher named. An app launched
/// into no workspace asks as itself, with no workspace's grant behind it.
fn caller(handoff: &Handoff) -> PortalCaller {
    PortalCaller {
        workspace: handoff.workspace_id.clone(),
        ..PortalCaller::new(handoff.app_id.clone())
    }
}

impl Prompter for ConsentsPrompter {
    fn granted(&self, handoff: &Handoff, grant: Grant) -> bool {
        self.consents
            .allowed(&caller(handoff), grant.device_scope())
    }

    fn ask(&self, handoff: &Handoff, grant: Grant) -> bool {
        let caller = caller(handoff);
        let question = tilo_portal::consent::AccessQuestion {
            capability: grant.device_scope().to_owned(),
            reason: None,
            may_remember: true,
            text: None,
        };
        let handle = format!("system-stats:{}:{}", caller.app_id, question.capability);
        // This connection's own thread waits; the service and the session
        // go on, and the sheet is shown on the loop's next pass.
        pollster::block_on(self.consents.decide(&caller, &handle, question)).is_ok()
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
    use super::{ConnectionKind, Duration, Map, Reading, Services, Started, Value, mpsc};

    /// How often the counters are read while this device has a connection
    /// up.
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

    /// The worker reading this device's counters, and what it read since
    /// the loop's last pass.
    pub(super) struct Counting {
        due: mpsc::Receiver<Due>,
        boot: Option<String>,
        _worker: std::thread::JoinHandle<()>,
    }

    impl Counting {
        /// Start reading this device's counters, waking the loop with
        /// `wake` for each reading it counts. The worker ends with what it
        /// reads into.
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
                        let offset = chrono::Local::now().offset().local_minus_utc();
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
                let counted = services.invoke(&format!("settings:{SECTION}"), COUNT, &arguments);
                match counted {
                    Some(Ok(Started::Done(_) | Started::Accepted)) => {}
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
        fn day(&self) -> Option<chrono::NaiveDate> {
            settings_sections::network_usage::local_day(self.at, self.offset)
        }

        /// Whether `next` counts on another connection or day than this: a
        /// counter on another connection or gone, or another local day.
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
        /// Take in a reading; the readings to count now, in order, each with
        /// when it was taken and the local offset then.
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
            // What is on the counters when this session starts was used
            // before it asked, and the count that follows carries it forward:
            // only a change, or a quarter hour of counting, is due.
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

    /// The arguments of `count-data-used` for `readings`.
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

        /// Readings wait a quarter hour to be counted, but one before a
        /// change of connection is counted at once, ahead of the one after
        /// it. The first reading is where the session starts counting from.
        #[test]
        fn readings_are_counted_on_their_own_connection() {
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

        #[test]
        fn the_readings_are_the_verbs_arguments() {
            let arguments = arguments(&[on("wifi:a", 1_000)], Some("b"), 10, 3_600);
            assert_eq!(arguments["readings"][0]["connection"], "wifi:a");
            assert_eq!(arguments["readings"][0]["kind"], "wifi");
            assert_eq!(arguments["readings"][0]["received"], 1_000);
            assert_eq!(arguments["boot"], "b");
            assert_eq!(arguments["offset"], 3_600);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{Owner, Reach, Service, TABLE};

    /// The launcher's sources that route a client to a service, and the
    /// protocol crates whose constants name one it routes.
    const SOURCES: &[(&str, &str)] = &[
        (
            "packaging/launcher/src/services.rs",
            include_str!("../../../../../crates/packaging/launcher/src/services.rs"),
        ),
        (
            "packaging/launcher/src/main.rs",
            include_str!("../../../../../crates/packaging/launcher/src/main.rs"),
        ),
        (
            "packaging/launcher/src/system_stats.rs",
            include_str!("../../../../../crates/packaging/launcher/src/system_stats.rs"),
        ),
        (
            "system/system-stats/src/protocol.rs",
            include_str!("../../../../../crates/system/system-stats/src/protocol.rs"),
        ),
        (
            "platform/tilo-render-protocol/src/broker.rs",
            include_str!("../../../../../crates/platform/tilo-render-protocol/src/broker.rs"),
        ),
    ];

    /// The names every `const NAME: &str = "…"` of the sources defines, by
    /// the constant's name: two protocol crates may both call their service
    /// name `SERVICE`, and a call site names both.
    fn constants() -> BTreeMap<String, BTreeSet<String>> {
        let mut constants: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (_, source) in SOURCES {
            for line in source.lines() {
                let line = line.trim();
                let Some(rest) = line
                    .strip_prefix("pub const ")
                    .or_else(|| line.strip_prefix("const "))
                else {
                    continue;
                };
                let Some((name, value)) = rest.split_once(": &str = ") else {
                    continue;
                };
                let value = value.trim_end_matches(';').trim();
                let Some(value) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
                    continue;
                };
                constants
                    .entry(name.trim().to_owned())
                    .or_default()
                    .insert(value.to_owned());
            }
        }
        constants
    }

    /// The names a source dials with `socket_for`: its own argument when it
    /// is a literal, and every name the constant it names is defined as
    /// otherwise. A bare identifier is a name that came from somewhere else
    /// (a manifest's declaration, handed in by the loop), which [`declared`]
    /// reads.
    fn dialled(source: &str, constants: &BTreeMap<String, BTreeSet<String>>) -> Vec<String> {
        let mut names = Vec::new();
        let mut rest = source;
        while let Some((_, after)) = rest.split_once("socket_for(") {
            rest = after;
            let Some(argument) = rest.split(')').next() else {
                break;
            };
            let argument = argument.trim();
            if let Some(literal) = argument.strip_prefix('"') {
                if let Some(name) = literal.split('"').next() {
                    names.push(name.to_owned());
                }
                continue;
            }
            let bare = argument
                .rsplit("::")
                .next()
                .unwrap_or(argument)
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
            if let Some(resolved) = constants.get(bare) {
                names.extend(resolved.iter().cloned());
            }
        }
        names
    }

    /// The names under `[sandbox.services]` of every app's manifest.
    fn declared() -> Vec<String> {
        let apps = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
        let mut manifests = std::fs::read_dir(&apps)
            .expect("the workspace's apps are readable")
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("tilo-app.toml"))
            .filter(|manifest| manifest.is_file())
            .collect::<Vec<_>>();
        manifests.sort();
        assert!(
            manifests.len() > 40,
            "only {} app manifests found",
            manifests.len()
        );
        let mut names = Vec::new();
        for manifest in manifests {
            let source = std::fs::read_to_string(&manifest).expect("an app manifest is readable");
            names.extend(manifest_services(&source));
        }
        names
    }

    /// The names a manifest declares under `[sandbox.services]`.
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

    /// The names a client in this tree dials: what the launcher hands at a
    /// launch, and what every app declares.
    fn dialled_by_clients() -> BTreeSet<String> {
        let constants = constants();
        let mut dialled: BTreeSet<String> = SOURCES
            .iter()
            .flat_map(|(_, source)| dialled(source, &constants))
            .collect();
        dialled.extend(declared());
        dialled
    }

    /// A service a client dials is never silently absent from this session:
    /// every name the launcher routes or an app declares has a row saying
    /// who serves it, or why nothing does. A service this module owns is
    /// started by `start`, whose match over `Service` is exhaustive.
    #[test]
    fn every_service_a_client_dials_has_a_row() {
        let missing: Vec<String> = dialled_by_clients()
            .into_iter()
            .filter(|name| !TABLE.iter().any(|door| door.name == name))
            .collect();
        assert!(
            missing.is_empty(),
            "no row serves {missing:?}: add one to the table in mount/doors.rs"
        );
    }

    /// Two rows for one name would be two servers on one socket: the table
    /// names each service once.
    #[test]
    fn the_table_names_each_service_once() {
        let mut seen = BTreeSet::new();
        for door in TABLE {
            assert!(
                seen.insert(door.name),
                "`{}` has more than one row",
                door.name
            );
        }
    }

    /// A row this module starts reaches its clients, a row served elsewhere
    /// names where, and a row nothing serves says why.
    #[test]
    fn every_row_says_how_it_is_reached_and_why_not() {
        for door in TABLE {
            match door.owner {
                Owner::Session(service) => assert_ne!(
                    door.reach,
                    Reach::NoConnection,
                    "`{}` is started but reached nowhere ({service:?})",
                    door.name
                ),
                Owner::Module(module) => assert!(
                    !module.is_empty() && door.reach != Reach::Plane,
                    "`{}` names the module `{module}` but is reached on the plane",
                    door.name
                ),
                Owner::Elsewhere(program) => assert!(
                    !program.is_empty() && door.reach == Reach::Launcher,
                    "`{}` names `{program}` as its server on no socket",
                    door.name
                ),
                Owner::Nothing(reason) => assert!(
                    reason.len() > 20,
                    "`{}` says nothing of why nothing serves it",
                    door.name
                ),
            }
        }
    }

    /// Every service [`super::start`] starts has a row, so the table is
    /// what a client's reader sees, and nothing is started outside it.
    #[test]
    fn every_started_service_has_a_row() {
        for service in [
            Service::Identity,
            Service::SystemInfo,
            Service::DataUsage,
            Service::Tools,
            Service::Render,
        ] {
            assert!(
                TABLE
                    .iter()
                    .any(|door| door.owner == Owner::Session(service)),
                "{service:?} has no row"
            );
        }
    }

    /// The derivation reads code, so a source or manifest it cannot read
    /// fails here rather than passing an empty set.
    #[test]
    fn the_dialled_names_come_from_the_code_that_dials_them() {
        let names = dialled_by_clients();
        for known in [
            launcher::services::COLLAB_SERVICE,
            launcher::services::IDENTITY_SERVICE,
            launcher::services::CHOOSER_SERVICE,
            launcher::services::COMPAT_SERVICE,
            tilo_system_stats::SERVICE,
            tilo_render_protocol::broker::SERVICE,
            "tools",
            "models",
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
