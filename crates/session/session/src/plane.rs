//! The session's side of the app wire: the resources its services publish,
//! read, watched and invoked by the apps it runs.
//!
//! An app is a process of its own, and the socket it speaks to its session on
//! is the launcher's: [`tilo_action_broker`] serves it, admits the peer by
//! the handoff the launcher stamped on the connection, and keeps the rooms
//! the apps share. What this session adds to that broker is one provider:
//! what its services publish ([`Link`], as the broker's
//! [`PublishedResources`]). Nothing of the wire itself is served here.
//!
//! The two sides run on two threads. The broker has a runtime of its own; the
//! services belong to the session's loop, the only thread that may touch
//! them, because a service blocks on its device and keeps its worker. [`Link`]
//! is the seam: an app's call becomes an [`Ask`] the loop answers on its next
//! pass, and a watched resource's reading moves to the apps watching it there.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime_collab::wire::{Event, Receipt};
use serde_json::Map;
use tilo_action_broker::collab_broker::{BrokerChannels, BrokerHandle, FanoutState};
use tilo_action_broker::collab_outbound::PeerPublisher;
use tilo_action_broker::services::{
    Caller, HostServices, PublishedResources, Reading, ResourceWatch, SpeakerChange,
    SpeakerServices,
};
use tilo_lock::MutexRecover as _;
use tilo_service::{Services, Started, Wake};
use tokio::sync::{mpsc, oneshot};

use crate::launches::Launches;

/// A watch the loop keeps: the resource an app connection watches, and what
/// that connection was last sent.
struct Watch {
    id: ResourceWatch,
    resource: String,
    /// The services' revision this watch was last served at.
    revision: Option<u64>,
    /// What it was last sent, so a change elsewhere does not re-send it.
    reading: Option<Vec<u8>>,
    sink: mpsc::UnboundedSender<Event>,
}

/// What a connection asked, waiting for the loop's pass.
enum Ask {
    Snapshot {
        resource: String,
        reply: oneshot::Sender<Reading>,
    },
    Invoke {
        caller: Caller,
        resource: String,
        action: String,
        /// The arguments, as the JSON object the wire carried.
        arguments: Vec<u8>,
        reply: oneshot::Sender<Receipt>,
    },
}

/// The seam between the session's loop and the broker's connections: what an
/// app asks of the services, and the readings the apps watch.
pub struct Link {
    asks: Mutex<Vec<Ask>>,
    watches: Mutex<Vec<Watch>>,
    next_watch: AtomicU64,
    wake: Wake,
}

impl Link {
    /// A link that wakes the session's loop with `wake`.
    #[must_use]
    pub fn new(wake: Wake) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            asks: Mutex::new(Vec::new()),
            watches: Mutex::new(Vec::new()),
            next_watch: AtomicU64::new(1),
            wake,
        })
    }

    fn ask(&self, ask: Ask) {
        self.asks.lock_recover().push(ask);
        (self.wake)();
    }

    /// Answer what the apps asked since the last pass, and send every
    /// watched resource's reading that moved: what the session's loop calls
    /// once per pass, after the services took in what their devices did.
    pub fn serve(&self, services: &mut Services) {
        let asked = std::mem::take(&mut *self.asks.lock_recover());
        for ask in asked {
            match ask {
                Ask::Snapshot { resource, reply } => {
                    let reading = services.read(&resource).map(|value| encoded(&value));
                    let _ = reply.send((services.revision(), reading));
                }
                Ask::Invoke {
                    caller,
                    resource,
                    action,
                    arguments,
                    reply,
                } => {
                    tracing::debug!(
                        app = caller.app_id,
                        workspace = caller.workspace,
                        %resource,
                        %action,
                        "an app invokes a published action"
                    );
                    let _ = reply.send(receipt(services, &resource, &action, &arguments));
                }
            }
        }
        self.publish(services);
    }

    /// Send the readings of the watched resources that moved.
    fn publish(&self, services: &Services) {
        let revision = services.revision();
        let mut watches = self.watches.lock_recover();
        watches.retain_mut(|watch| {
            if watch.revision == Some(revision) {
                return true;
            }
            let reading = services.read(&watch.resource).map(|value| encoded(&value));
            watch.revision = Some(revision);
            if reading == watch.reading && watch.reading.is_some() {
                return true;
            }
            watch.reading.clone_from(&reading);
            watch
                .sink
                .send(Event::Reading {
                    resource: watch.resource.clone(),
                    revision,
                    reading,
                })
                .is_ok()
        });
    }
}

/// The session's services, as the broker's connections read them: a resource
/// is named by its id, and what an app reaches is what the services publish.
impl PublishedResources for Link {
    fn snapshot(&self, _caller: &Caller, resource: &str) -> oneshot::Receiver<Reading> {
        let (reply, answered) = oneshot::channel();
        self.ask(Ask::Snapshot {
            resource: resource.to_owned(),
            reply,
        });
        answered
    }

    fn watch(
        &self,
        _caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<Event>,
    ) -> ResourceWatch {
        let id = ResourceWatch::new(self.next_watch.fetch_add(1, Ordering::Relaxed));
        self.watches.lock_recover().push(Watch {
            id,
            resource: resource.to_owned(),
            revision: None,
            reading: None,
            sink,
        });
        (self.wake)();
        id
    }

    fn unwatch(&self, id: ResourceWatch) {
        self.watches.lock_recover().retain(|watch| watch.id != id);
    }

    fn invoke(
        &self,
        caller: &Caller,
        resource: &str,
        action: &str,
        arguments: Vec<u8>,
    ) -> oneshot::Receiver<Receipt> {
        let (reply, answered) = oneshot::channel();
        self.ask(Ask::Invoke {
            caller: caller.clone(),
            resource: resource.to_owned(),
            action: action.to_owned(),
            arguments,
            reply,
        });
        answered
    }
}

/// The speaker groups this session's fabric carries, as the broker's
/// Settings route commands them: Settings asks for a snapshot, creates,
/// edits and calibrates a group, and follows their changes, and the speaker
/// service answers from the fabric the loop pumps.
struct FabricSpeakers(tilo_device_fabric::link::SharedLink);

impl SpeakerServices for FabricSpeakers {
    fn command(&self, command: runtime_collab::SpeakerCommand) -> runtime_collab::SpeakerReply {
        tilo_speaker_service::settings::handle(&self.0, command)
    }

    fn changed(&self, after: u64) -> Option<SpeakerChange> {
        Some(Box::pin(tilo_speaker_service::settings::changed(
            &self.0, after,
        )))
    }
}

/// Serve the session's published resources to the apps on `socket`: the
/// broker admits each connection by the launch `launches` recorded, asks
/// `link` for what the services publish, and hands Settings the speaker
/// groups of `fabric` when this session runs on one.
///
/// # Errors
///
/// The socket's directory is not this user's private directory, another
/// session answers on the socket, or the broker's thread could not start.
pub fn serve(
    socket: PathBuf,
    link: std::sync::Arc<Link>,
    launches: std::sync::Arc<Launches>,
    fabric: Option<tilo_device_fabric::link::SharedLink>,
    live_activities: std::sync::Arc<runtime_collab::LiveActivities>,
) -> std::io::Result<BrokerHandle> {
    let services = host_services(link, launches, fabric, live_activities);
    tilo_action_broker::collab_broker::spawn_with_services(
        socket,
        None,
        PeerPublisher::disabled(),
        None,
        services,
        std::sync::Arc::new(Mutex::new(BrokerChannels::default())),
        std::sync::Arc::new(FanoutState::new()),
    )
}

/// What the apps this session runs reach of it: the resources its services
/// publish, the launcher that started them, and — when this session runs on a
/// device fabric — the speaker groups Settings asks that fabric for.
fn host_services(
    link: std::sync::Arc<Link>,
    launches: std::sync::Arc<Launches>,
    fabric: Option<tilo_device_fabric::link::SharedLink>,
    live_activities: std::sync::Arc<runtime_collab::LiveActivities>,
) -> HostServices {
    let speakers: Option<std::sync::Arc<dyn SpeakerServices>> = fabric.map(|fabric| {
        std::sync::Arc::new(FabricSpeakers(fabric)) as std::sync::Arc<dyn SpeakerServices>
    });
    HostServices {
        published: Some(link),
        launcher: Some(launches),
        speakers,
        // Every app shows its live activity (a running timer, a stopwatch)
        // outside its windows on this one board.
        live_activities: Some(live_activities),
        ..HostServices::default()
    }
}

/// A value as the wire carries it: its JSON.
fn encoded(value: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

/// What invoking one of the services' actions answered.
fn receipt(services: &mut Services, resource: &str, action: &str, arguments: &[u8]) -> Receipt {
    let arguments = match serde_json::from_slice::<Map<String, serde_json::Value>>(arguments) {
        Ok(arguments) => arguments,
        Err(error) => {
            return Receipt::Refused {
                code: "invalid_arguments".into(),
                message: format!("the arguments are not a JSON object: {error}"),
            };
        }
    };
    match services.invoke(resource, action, &arguments) {
        Some(Ok(Started::Done(output))) => match serde_json::to_vec(&output) {
            Ok(output) => Receipt::Done(output),
            Err(error) => Receipt::Refused {
                code: "failed".into(),
                message: format!("the action's output does not encode: {error}"),
            },
        },
        Some(Ok(Started::Accepted)) => Receipt::Accepted,
        Some(Err(error)) => Receipt::Refused {
            code: error.code().to_owned(),
            message: error.to_string(),
        },
        None => Receipt::Refused {
            code: "unknown_action".into(),
            message: format!("this session publishes no `{resource}` resource"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_device_fabric::link::{FabricLink, PersonalFabricSendError};
    use tilo_device_fabric::speaker_group::{
        AudioEndpoint, AudioEndpointKind, EndpointSelector, SpeakerFabricState,
    };
    use tilo_device_fabric::{
        DeviceFabric, DeviceFabricSnapshot, DeviceId, DeviceKind, DevicePresence,
        FABRIC_SCHEMA_VERSION, FabricCapabilities, FabricStamp, OwnerSession, PersonalFabricAction,
    };

    /// This laptop's device fabric, with the one output it publishes.
    struct Fabric {
        local: DeviceId,
        converged: DeviceFabric,
    }

    impl Fabric {
        fn laptop() -> Self {
            let local = DeviceId::new("this-laptop").expect("a device id");
            let mut converged = DeviceFabric::default();
            converged
                .upsert(DeviceFabricSnapshot {
                    schema_version: FABRIC_SCHEMA_VERSION,
                    owner: local.clone(),
                    presence: DevicePresence {
                        device: local.clone(),
                        label: "This laptop".into(),
                        kind: DeviceKind::Laptop,
                        capabilities: FabricCapabilities {
                            audio_output: true,
                            audio_group: true,
                            ..FabricCapabilities::default()
                        },
                        online: true,
                        last_seen_unix_ms: 1,
                        wake_routes: Vec::new(),
                        session: OwnerSession::default(),
                        stamp: FabricStamp {
                            counter: 1,
                            device: local.clone(),
                        },
                    },
                    media: Vec::new(),
                    clipboard: Vec::new(),
                    settings: Vec::new(),
                    wifi: Vec::new(),
                    display: None,
                    speakers: SpeakerFabricState {
                        endpoints: vec![AudioEndpoint {
                            selector: EndpointSelector::mint(&local, "alsa_output.analog-stereo"),
                            kind: AudioEndpointKind::Output,
                            label: "Speakers".into(),
                            is_default: true,
                        }],
                        ..SpeakerFabricState::default()
                    },
                    unlock: tilo_device_fabric::unlock::UnlockFabricState::default(),
                })
                .expect("the snapshot is valid");
            Self { local, converged }
        }
    }

    impl FabricLink for Fabric {
        fn device_fabric(&self) -> DeviceFabric {
            self.converged.clone()
        }

        fn online_device_ids(&self) -> std::collections::BTreeSet<DeviceId> {
            std::collections::BTreeSet::from([self.local.clone()])
        }

        fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
            Some((self.local.clone(), "This laptop".into()))
        }

        fn send_personal_fabric_action(
            &self,
            _: &DeviceId,
            _: PersonalFabricAction,
        ) -> Result<[u8; 16], String> {
            Err("no transport in a test".into())
        }

        fn try_send_personal_fabric_action(
            &self,
            _: &DeviceId,
            _: PersonalFabricAction,
        ) -> Result<[u8; 16], PersonalFabricSendError> {
            Err(PersonalFabricSendError::Rejected("no transport".into()))
        }

        fn require_media(&self) -> Result<(), String> {
            Ok(())
        }
    }

    fn services(fabric: Option<tilo_device_fabric::link::SharedLink>) -> HostServices {
        let wake: Wake = std::sync::Arc::new(|| ());
        let link = Link::new(wake);
        host_services(
            link,
            std::sync::Arc::new(Launches::default()),
            fabric,
            std::sync::Arc::new(runtime_collab::LiveActivities::default()),
        )
    }

    /// Settings reaches the speaker groups of the fabric this session runs
    /// on, and of nothing else: a session without one has no speaker route.
    #[test]
    fn the_session_answers_settings_speaker_route_from_its_fabric() {
        assert!(
            services(None).speakers.is_none(),
            "a session on no fabric carries no speaker groups"
        );

        let speakers = services(Some(std::sync::Arc::new(Fabric::laptop())))
            .speakers
            .expect("the speaker route is registered");
        let runtime_collab::SpeakerReply::Snapshot { snapshot } =
            speakers.command(runtime_collab::SpeakerCommand::Snapshot)
        else {
            panic!("Settings asks for a snapshot");
        };
        assert!(snapshot.media_enabled);
        let labels = snapshot
            .devices
            .iter()
            .map(|device| device.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, ["This laptop"]);
        assert_eq!(
            snapshot.devices[0].outputs.len(),
            1,
            "the output this laptop publishes is what Settings can put in a set"
        );
        assert!(
            speakers.changed(0).is_some(),
            "and it tells Settings when that changes"
        );
    }
}
