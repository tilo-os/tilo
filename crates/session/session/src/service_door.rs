//! The session's door to its services: the system services' published side, and a surface
//! app's way to the desktop's actions, as one action host.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_service::{Service, Services, Started};
use tilo_surface_host::{Doors, Published};
use ui::host_operation::ResourceOperationStart;

use crate::surfaces;

/// The system services' published side, behind the session's door like
/// every hosted surface: one action host for all of them.
struct ServiceDoor {
    services: Rc<RefCell<Services>>,
    /// The last reading asked for, and the services' revision it was read
    /// at: a panel following a resource is answered from here until the
    /// services move, so a frame never reads the device again.
    last: (u64, Option<(String, serde_json::Value)>),
}

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
