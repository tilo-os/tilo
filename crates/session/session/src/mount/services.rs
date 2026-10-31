//! The system services, and the door apps reach this session's actions
//! through: one published side for all of them, and the plane they read it on.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::Error;
use tilo_service::Started;
use tilo_surface_host::{Doors, Published};
use ui::host_operation::ResourceOperationStart;

use crate::{ActionPlane, Options, Plane, Service, Services, plane, surfaces};

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
    /// The last reading asked for, and the services' revision it was read
    /// at: a panel following a resource is answered from here until the
    /// services move, so a frame never reads the device again.
    last: (u64, Option<(String, serde_json::Value)>),
}

/// A surface app reaches the desktop's actions exactly as a person's
/// palette does: through the door over every surface it hosts.
impl surfaces::ActionDoor for tilo_surface_host::Doors {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        tilo_surface_host::Doors::resources(self)
    }

    fn read(&mut self, resource: &str) -> Option<serde_json::Value> {
        tilo_surface_host::Doors::read(self, resource)
    }

    fn revision(&mut self) -> Option<u64> {
        tilo_surface_host::Doors::moved(self)
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

    fn read(&mut self, resource: &str) -> Option<serde_json::Value> {
        let revision = self.services.borrow().revision();
        if self.last.0 == revision
            && let Some((read, value)) = &self.last.1
            && read == resource
        {
            return Some(value.clone());
        }
        let value = self.services.borrow().read(resource)?;
        self.last = (revision, Some((resource.to_owned(), value.clone())));
        Some(value)
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
    let broker = plane::serve(
        path,
        std::sync::Arc::clone(&link),
        std::sync::Arc::clone(&options.launches),
        options.fabric.clone(),
        std::sync::Arc::clone(live_activities),
        // The index search reads is the one apps publish into: a closed
        // app's content is found from here on.
        options.search.content.clone(),
    )?;
    Ok(Some(Plane {
        link,
        _broker: broker,
    }))
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
        last: (0, None),
    })));
    services
}
