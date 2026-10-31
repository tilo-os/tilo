//! The system services, and the door apps reach this session's actions
//! through: one published side for all of them, and the plane they read it on.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::Error;
use tilo_service::Started;
use tilo_surface_host::{Doors, Published};
use ui::host_operation::ResourceOperationStart;

use crate::plane::{DeskAuthority, ShortcutLibrary};
use crate::{
    ActionPlane, DesktopHandle, Options, Plane, Service, Services, activity, activity_provider,
    plane, surfaces,
};

/// What a service answered, as a caller of the action protocol reads it.
pub(crate) fn answered(started: Started) -> ResourceOperationStart {
    match started {
        Started::Done(output) => ResourceOperationStart::Completed(output),
        Started::Accepted => ResourceOperationStart::Accepted,
    }
}

/// The system services' published side, behind the session's door like
/// every hosted surface: one action host for all of them.
struct ServiceDoor {
    services: Rc<RefCell<Services>>,
}

/// A surface app reaches the desktop's actions exactly as a person's
/// palette does: through the door over every surface it hosts.
impl surfaces::ActionDoor for tilo_surface_host::Doors {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        tilo_surface_host::Doors::resources(self)
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        tilo_surface_host::Doors::invoke(self, address, arguments)
    }
}

impl Published for ServiceDoor {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        self.services.borrow().resources()
    }

    /// The lock screen asks the session through here while it is locked, so
    /// the services stay behind the door; only the lock's own resource is
    /// offered then.
    fn serves_while_locked(&self) -> bool {
        true
    }

    fn locked_resources(&mut self) -> Vec<ResourceSpec> {
        self.services
            .borrow()
            .resources()
            .into_iter()
            .filter(|resource| resource.id.as_str() == crate::lock_service::LOCK)
            .collect()
    }

    fn revision(&self) -> Option<u64> {
        Some(self.services.borrow().revision())
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        match self
            .services
            .borrow_mut()
            .invoke(&address.resource, &address.action, arguments)
        {
            Some(Ok(Started::Done(output))) => Ok(ResourceOperationStart::Completed(output)),
            Some(Ok(Started::Accepted)) => Ok(ResourceOperationStart::Accepted),
            Some(Err(error)) => Err(error),
            None => Err(DoorError::Unknown(address.clone())),
        }
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// The plane the apps read and invoke this session's services on, when it
/// serves one: the socket the launcher hands its apps, and the link the loop
/// answers their calls through.
pub(crate) fn start_action_plane(
    options: &Options,
    live_activities: &std::sync::Arc<runtime_collab::LiveActivities>,
    handle: &DesktopHandle,
    shortcuts: std::sync::Arc<ShortcutLibrary>,
    activity: &activity::Activity,
    agent_runs: std::sync::Arc<tilo_agent_runs::AgentRuns>,
    revocations: crate::engine::Revocations,
    sync: Option<&crate::sync::Client>,
) -> Result<Option<Plane>, Error> {
    let path = match &options.action_plane {
        ActionPlane::Off => return Ok(None),
        ActionPlane::Launcher => {
            // The launcher hands an app the socket under the services root,
            // which only its owner may reach.
            launcher::services::ensure_services_root()?;
            launcher::services::collab_socket()
        }
        ActionPlane::At(path) => path.clone(),
    };
    let link = plane::Link::new(options.wake.handle());
    // What the broker decides against, from this pass on: the desk as it is
    // before the person touches anything, and every desk it shows after.
    let desk = std::sync::Arc::new(DeskAuthority::new(handle.read(Clone::clone), revocations));
    // The workspace's activity, as the apps this session runs read it: the
    // same model the desk serves its own windows.
    let activity_wire = activity_provider::wire(activity.requests());
    activity.serve_apps(std::sync::Arc::clone(&activity_wire));
    // Calls run over the collaboration plane: an app joins a channel through
    // the service, the session says the channel in its presence, and what the
    // room says of its calls is what the app's channels show.
    let calls = crate::calls::Calls::new();
    let calls_service = sync.map(|sync| {
        calls.follow(sync, None);
        let service = tilo_native_media::NativeMediaService::new();
        service.install_factory(std::sync::Arc::new(crate::calls::Factory::new(
            std::sync::Arc::clone(&calls),
        )));
        service
    });
    let broker = plane::serve(
        path.clone(),
        std::sync::Arc::clone(&link),
        std::sync::Arc::clone(&options.launches),
        options.fabric.clone(),
        plane::Hosted {
            live_activities: std::sync::Arc::clone(live_activities),
            native_media: calls_service.clone(),
            // The index search reads is the one apps publish into: a closed
            // app's content is found from here on.
            content: options.search.content.clone(),
            desk: std::sync::Arc::clone(&desk),
            shortcuts,
            activity: activity_wire,
            // The runs the broker delivers to the agents admitted here, and
            // hears back what they report of them.
            agent_runs: Some(agent_runs),
        },
    )?;
    Ok(Some(Plane {
        link,
        path,
        desk,
        broker,
        calls,
        calls_service,
    }))
}

/// The person's private messages, published beside the other services: the
/// conversations this device keeps and the action that sends a text. It
/// answers peers on this session's own endpoint, so it is mounted with the
/// collaboration plane and only there.
pub(crate) fn mount_messages(
    options: &mut Options,
    sync: Option<&crate::sync::Client>,
) -> Option<crate::messages::Messages> {
    let sync = sync?;
    let service = match crate::messages::Messages::new(&options.wake) {
        Ok(service) => service,
        Err(error) => {
            tracing::warn!(%error, "private messages do not start");
            return None;
        }
    };
    if let Some(identity) = crate::sync::device_identity() {
        service.act_as(identity, hex::encode(sync.publisher().as_bytes()));
    }
    service
        .reached_at(serde_json::to_value(sync.endpoint_addr()).unwrap_or(serde_json::Value::Null));
    // The lane this device's endpoint carries, bound when the peer actor
    // hands it over: until then a send says this device carries none.
    if let Some(answer) = sync.private_messages() {
        let binding = service.clone();
        let thread = std::thread::Builder::new()
            .name("tilo-messages-bind".into())
            .spawn(move || {
                match answer.blocking_recv() {
                    Ok(transport) => {
                        if let Err(error) = binding.bind(transport) {
                            tracing::warn!(?error, "private messages answer no peer");
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "no endpoint carries private messages");
                    }
                }
                binding.wake();
            });
        if let Err(error) = thread {
            tracing::warn!(%error, "private messages answer no peer");
        }
    }
    options.services.push(Box::new(service.clone()));
    Some(service)
}

/// The services, published on the door.
pub(crate) fn mount_services(
    doors: &mut Doors,
    started: Vec<Box<dyn Service>>,
) -> Rc<RefCell<Services>> {
    let mut services = Services::default();
    for service in started {
        services.add(service);
    }
    let services = Rc::new(RefCell::new(services));
    doors.add(Rc::new(RefCell::new(ServiceDoor {
        services: Rc::clone(&services),
    })));
    services
}
