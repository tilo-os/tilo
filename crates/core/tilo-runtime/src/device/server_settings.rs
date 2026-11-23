//! This device's server settings, as the daemon publishes them and applies
//! them: the work it takes from the person's other devices and the services it
//! offers them (`device:server`).
//!
//! The resource is the daemon's because the configuration behind it is: a
//! machine takes work through the server configuration [`keep`](super::keep)
//! reads, which lives above the person's own files. Settings shows the reading
//! and changes it by its actions; the daemon applies each change to that
//! configuration and saves it where it lives, and every view watching the
//! resource reads the new setting.
//!
//! One implementation serves both hosts of a device: the daemon's broker,
//! where an app on a server attaches ([`Published`]), and a desktop session's
//! service list, where an app on a person's own computer attaches
//! ([`Served`] as a [`Service`]). What it writes is the same configuration file
//! either way.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};
use tilo_lock::MutexRecover as _;
use tilo_service::{Service, Started, decode, offer, reading};
use tilo_system_resources::device_settings::{
    ServerSettings, ServerSettingsAction, ServerSettingsReading, Services, Work, WorkFrom, WorkWhen,
};

use tilo_actions::{ActionOutput, ApiError, ResourceSpec};
use tilo_workspace_service::apps::services::{Caller, PublishedResources, Reading, ResourceWatch};
use tokio::sync::{mpsc, oneshot};

/// Where a device that has no server configuration yet keeps the one its person
/// sets up.
const DEFAULT_CONFIG: &str = super::keep::CONFIG;

/// The resource this module serves.
pub use tilo_system_resources::device_settings::SERVER;

/// The application whose window shows and changes this device's server
/// settings: the only caller these settings are read and changed for. A device's
/// server configuration decides what its machine runs for the person's other
/// devices, so it is not any app's to read or change.
const SETTINGS_APP: &str = tilo_app_id::apps::SETTINGS;

/// This device's server settings, read from its configuration.
pub struct Served {
    path: PathBuf,
    /// The configuration and the reading as they stand, as the actions and the
    /// reading itself change them.
    current: ServerSettings,
    config: tilo_server_protocol::config::ServerConfig,
    /// Moves with every change of the reading.
    revision: u64,
}

impl Served {
    /// This device's server settings: the configuration it has, or the one it
    /// keeps when its person first changes a setting.
    #[must_use]
    pub fn this_device() -> Self {
        Self::at(super::keep::config().unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG)))
    }

    /// The settings a device keeps at `path`.
    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        let (config, problem) = match tilo_server_protocol::config::ServerConfig::load(&path) {
            Ok(config) => (config, None),
            Err(tilo_server_protocol::config::ConfigError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                let config = tilo_server_protocol::config::ServerConfig {
                    work: tilo_server_protocol::WorkSetting::none(),
                    ..tilo_server_protocol::config::ServerConfig::default()
                };
                (config, None)
            }
            Err(error) => {
                tracing::warn!(
                    target: "tilo::device",
                    %error,
                    config = %path.display(),
                    "this device's server settings could not be read"
                );
                (
                    tilo_server_protocol::config::ServerConfig {
                        work: tilo_server_protocol::WorkSetting::none(),
                        ..tilo_server_protocol::config::ServerConfig::default()
                    },
                    Some(format!("This device's settings could not be read: {error}")),
                )
            }
        };
        let configured = path.is_file();
        let current = settings_of(&config, configured, problem);
        Self {
            path,
            current,
            config,
            revision: 1,
        }
    }

    /// The reading as it stands.
    #[must_use]
    pub fn reading(&self) -> ServerSettings {
        self.current.clone()
    }

    /// The path this device's settings are saved to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The revision the reading stands at.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The `device:server` resource as it is now.
    fn resource() -> ResourceSpec {
        offer::<ServerSettingsAction>(tilo_resource::typed_resource::<ServerSettingsReading>(
            SERVER,
            "Server settings",
        ))
    }

    /// Apply `action` of the resource with `arguments` to this device's
    /// configuration, and save it where it lives.
    ///
    /// # Errors
    /// The action is unknown, its arguments do not name a setting, or the
    /// configuration could not be saved: the setting stays as it was.
    pub fn apply(&mut self, action: &str, arguments: &Map<String, Value>) -> Result<(), ApiError> {
        let published = [Self::resource()];
        let effect = decode::<ServerSettingsAction>(&published, SERVER, action, arguments)?;
        let mut current = self.current.clone();
        let mut config = self.config.clone();
        match effect {
            ServerSettingsAction::SetWork { work } => {
                let from = WorkFrom::named(&work).ok_or_else(|| {
                    ApiError::InvalidArguments(format!("{work} is no work this device can take"))
                })?;
                let was_taking = current.work.from != WorkFrom::Nobody;
                current.set_work(from);
                config.work.from = protocol_from(from);
                // A device that takes no work runs no service for others, and one that does takes
                // the workspace's traffic: what the person's own devices' work needs. The services
                // they set one by one are left as they are.
                let takes = from != WorkFrom::Nobody;
                if takes != was_taking {
                    config.services.workspace_replication = takes;
                    config.services.inbound_server = takes;
                }
            }
            ServerSettingsAction::SetWorkWhileCharging { on } => {
                current.set_work_while_charging(on);
                config.work.when.charging_only = on;
            }
            ServerSettingsAction::SetWorkRoom { part, percent } => {
                current
                    .set_work_room(&part, percent)
                    .map_err(ApiError::InvalidArguments)?;
                match part.as_str() {
                    "processors" => config.work.when.free_cpu_percent = percent,
                    "memory" => config.work.when.free_memory_percent = percent,
                    _ => {
                        return Err(ApiError::InvalidArguments(format!(
                            "{part} is no part of the machine"
                        )));
                    }
                }
            }
            ServerSettingsAction::SetService { service, on } => {
                let mut services = current.services;
                services
                    .set(&service, on)
                    .map_err(ApiError::InvalidArguments)?;
                current.services = services;
                match service.as_str() {
                    "workspace_replication" => config.services.workspace_replication = on,
                    "inbound_server" => config.services.inbound_server = on,
                    "remote_control_server" => config.services.remote_control_server = on,
                    "app_serving" => config.services.app_serving = on,
                    "compute" => config.services.compute = on,
                    _ => {
                        return Err(ApiError::InvalidArguments(format!(
                            "{service} is no service of this device"
                        )));
                    }
                }
            }
        }
        current.configured = true;
        current.state_root = config.state_root.display().to_string();
        current.problem = None;
        config.save(&self.path).map_err(|error| {
            tracing::warn!(
                target: "tilo::device",
                %error,
                config = %self.path.display(),
                "this device's server settings could not be saved"
            );
            ApiError::Failed(format!(
                "This device's settings could not be saved: {error}"
            ))
        })?;
        self.config = config;
        self.current = current;
        self.revision = self.revision.wrapping_add(1).max(1);
        Ok(())
    }
}

/// The reading of `config`.
fn settings_of(
    config: &tilo_server_protocol::config::ServerConfig,
    configured: bool,
    problem: Option<String>,
) -> ServerSettings {
    ServerSettings {
        configured,
        problem,
        state_root: config.state_root.display().to_string(),
        work: Work {
            from: contract_from(&config.work.from),
            when: WorkWhen {
                charging_only: config.work.when.charging_only,
                free_cpu_percent: config.work.when.free_cpu_percent,
                free_memory_percent: config.work.when.free_memory_percent,
            },
        },
        services: Services {
            workspace_replication: config.services.workspace_replication,
            inbound_server: config.services.inbound_server,
            remote_control_server: config.services.remote_control_server,
            app_serving: config.services.app_serving,
            compute: config.services.compute,
        },
    }
}

/// The work the person's own devices' and anyone's work is, as the setting
/// names it. Work taken from the workspaces a device names is taken from
/// others too, which is what the setting's "anyone" offers.
fn contract_from(from: &tilo_server_protocol::WorkFrom) -> WorkFrom {
    match from {
        tilo_server_protocol::WorkFrom::Nobody => WorkFrom::Nobody,
        tilo_server_protocol::WorkFrom::OwnDevices => WorkFrom::OwnDevices,
        tilo_server_protocol::WorkFrom::Workspaces(_) | tilo_server_protocol::WorkFrom::Anyone => {
            WorkFrom::Anyone
        }
    }
}

/// The configuration's work from, as [`contract_from`] read it.
fn protocol_from(from: WorkFrom) -> tilo_server_protocol::WorkFrom {
    match from {
        WorkFrom::Nobody => tilo_server_protocol::WorkFrom::Nobody,
        WorkFrom::OwnDevices => tilo_server_protocol::WorkFrom::OwnDevices,
        WorkFrom::Anyone => tilo_server_protocol::WorkFrom::Anyone,
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![Self::resource()]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == SERVER).then(|| reading(&self.current))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if resource != SERVER {
            return Err(ApiError::Unknown(tilo_actions::ActionAddress::new(
                resource, action,
            )));
        }
        self.apply(action, arguments)?;
        Ok(Started::Done(ActionOutput::default()))
    }
}

/// One app's watch of the resource: where its readings go, and the revision it
/// was last sent.
struct Watch {
    id: ResourceWatch,
    sink: mpsc::UnboundedSender<runtime_collab::wire::Event>,
    revision: u64,
}

/// This device's server settings as the daemon's broker publishes them to the
/// apps attached to it: the reading every view follows, and the actions that
/// change it, applied to the configuration this device keeps.
pub struct Published {
    served: Arc<std::sync::Mutex<Served>>,
    watches: std::sync::Mutex<Vec<Watch>>,
    next_watch: AtomicU64,
}

/// Why a caller may not read or change this device's server settings.
fn refused() -> String {
    "this device's server settings are Settings' to show".to_owned()
}

/// Whether `caller` may read and change this device's server settings. Settings'
/// window is the caller: the setting decides what the machine runs for the
/// person's other devices.
fn granted(caller: &Caller) -> bool {
    caller.app_id == SETTINGS_APP
}

impl Published {
    /// The daemon's view of `served`.
    #[must_use]
    pub fn new(served: Served) -> Self {
        Self {
            served: Arc::new(std::sync::Mutex::new(served)),
            watches: std::sync::Mutex::default(),
            next_watch: AtomicU64::new(1),
        }
    }

    /// The reading as it stands, encoded for the wire.
    fn encoded(&self) -> (u64, Option<Vec<u8>>) {
        let served = self.served.lock_recover();
        let reading = served
            .read(SERVER)
            .and_then(|reading| serde_json::to_vec(&reading).ok());
        (served.revision(), reading)
    }

    /// Send every watcher of the resource the reading as it stands now.
    fn publish(&self) {
        let (revision, reading) = self.encoded();
        let mut watches = self.watches.lock_recover();
        watches.retain_mut(|watch| {
            if watch.revision == revision {
                return true;
            }
            watch.revision = revision;
            watch
                .sink
                .send(runtime_collab::wire::Event::Reading {
                    resource: SERVER.to_owned(),
                    revision,
                    reading: reading.clone(),
                })
                .is_ok()
        });
    }
}

impl PublishedResources for Published {
    fn snapshot(
        &self,
        caller: &Caller,
        resource: &str,
    ) -> oneshot::Receiver<Result<Reading, String>> {
        let (reply, answered) = oneshot::channel();
        let answer = if resource != SERVER {
            Ok((0, None))
        } else if granted(caller) {
            Ok(self.encoded())
        } else {
            Err(refused())
        };
        let _ = reply.send(answer);
        answered
    }

    fn watch(
        &self,
        caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<ResourceWatch, String> {
        if resource != SERVER {
            return Err(format!("this device publishes no `{resource}` resource"));
        }
        if !granted(caller) {
            return Err(refused());
        }
        let (revision, reading) = self.encoded();
        let _ = sink.send(runtime_collab::wire::Event::Reading {
            resource: SERVER.to_owned(),
            revision,
            reading,
        });
        let id = ResourceWatch::new(self.next_watch.fetch_add(1, Ordering::Relaxed));
        self.watches
            .lock_recover()
            .push(Watch { id, sink, revision });
        Ok(id)
    }

    fn unwatch(&self, watch: ResourceWatch) {
        self.watches.lock_recover().retain(|held| held.id != watch);
    }

    fn invoke(
        &self,
        caller: &Caller,
        resource: &str,
        action: &str,
        arguments: Vec<u8>,
    ) -> oneshot::Receiver<runtime_collab::wire::Receipt> {
        let (reply, answered) = oneshot::channel();
        if resource != SERVER {
            let _ = reply.send(runtime_collab::wire::Receipt::Refused {
                code: "unknown_action".into(),
                message: format!("this device publishes no `{resource}` resource"),
            });
            return answered;
        }
        if !granted(caller) {
            let _ = reply.send(runtime_collab::wire::Receipt::Refused {
                code: "not_granted".into(),
                message: refused(),
            });
            return answered;
        }
        let arguments = match serde_json::from_slice::<Map<String, Value>>(&arguments) {
            Ok(arguments) => arguments,
            Err(error) => {
                let _ = reply.send(runtime_collab::wire::Receipt::Refused {
                    code: "invalid_arguments".into(),
                    message: format!("the arguments are not a JSON object: {error}"),
                });
                return answered;
            }
        };
        let answer = self.served.lock_recover().apply(action, &arguments);
        let receipt = match answer {
            Ok(()) => runtime_collab::wire::Receipt::Done(
                serde_json::to_vec(&ActionOutput::default()).unwrap_or_default(),
            ),
            Err(error) => runtime_collab::wire::Receipt::Refused {
                code: error.code().to_owned(),
                message: error.to_string(),
            },
        };
        self.publish();
        let _ = reply.send(receipt);
        answered
    }
}

#[cfg(test)]
#[path = "server_settings_tests.rs"]
mod tests;
