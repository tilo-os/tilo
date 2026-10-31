//! What a hosted surface publishes, and the one door the session reaches
//! every hosted surface through.
//!
//! Each surface (the desk, the chrome, quick settings, the banners, …) is
//! an ordinary widget tree or app with its own published resources and
//! actions. [`Doors`] gathers them: a caller of the action protocol (a
//! person's key, the palette, an agent, a test) names an action by address
//! and the door resolves it to the surface publishing it. Nothing calls a
//! surface directly.

use std::cell::RefCell;
use std::rc::Rc;

use ui::device_local_door::SharedDoor;
use ui::host_operation::ResourceOperationStart;
use ui::tilo_actions::{ActionAddress, DoorError, ResourceSpec};

/// A hosted surface's published side.
pub trait Published {
    /// What it publishes now: its resources, with their actions.
    fn resources(&mut self) -> Vec<ResourceSpec>;

    /// The reading of `resource` now, as its state's JSON; `None` when it
    /// publishes no such resource, or publishes nothing of it.
    fn read(&mut self, _resource: &str) -> Option<serde_json::Value> {
        None
    }

    /// Run its published action `address` with `arguments`.
    ///
    /// # Errors
    /// The surface's refusal: no such action, bad arguments, not allowed.
    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError>;

    /// The labels of its accessibility tree, in reading order.
    fn accessible_labels(&mut self) -> Vec<String>;

    /// Where what it publishes stands: equal revisions publish the same.
    /// `None` when it cannot tell, and is read again every time.
    fn revision(&self) -> Option<u64> {
        None
    }

    /// Whether it offers its actions now: a sheet only while it is up.
    fn offered(&self) -> bool {
        true
    }

    /// Whether, while offered, it withholds every other surface: setup
    /// over the whole desktop until the person finished it.
    fn exclusive(&self) -> bool {
        false
    }
}

/// Run `address` through `door` (the surface app's own door on this
/// device, which every caller shares), `serve` it at once, and take the
/// answer.
///
/// # Errors
/// The app's refusal, or none came: nothing there publishes `address`.
pub fn through_door(
    door: &SharedDoor,
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    serve: impl FnOnce(),
) -> Result<ResourceOperationStart, DoorError> {
    let answer = door.invoke(address.clone(), arguments.clone());
    serve();
    let answered = answer.borrow_mut().take();
    answered.unwrap_or_else(|| Err(DoorError::Unknown(address.clone())))
}

/// A hosted surface the session keeps beside the compositor's hold of it.
pub type Shared = Rc<RefCell<dyn Published>>;

/// Every hosted surface's published side, the session's action host for
/// them.
#[derive(Default, Clone)]
pub struct Doors {
    surfaces: Vec<Shared>,
}

impl Doors {
    /// Reach `surface` through the door from now on.
    pub fn add(&mut self, surface: Shared) {
        self.surfaces.push(surface);
    }

    /// The surfaces offering their actions now, with their place: only
    /// the exclusive ones while one is up.
    fn offered(&self) -> Vec<(usize, &Shared)> {
        let offered: Vec<(usize, &Shared)> = self
            .surfaces
            .iter()
            .enumerate()
            .filter(|(_, surface)| surface.borrow().offered())
            .collect();
        if offered
            .iter()
            .any(|(_, surface)| surface.borrow().exclusive())
        {
            offered
                .into_iter()
                .filter(|(_, surface)| surface.borrow().exclusive())
                .collect()
        } else {
            offered
        }
    }

    /// Where everything offered now stands: equal revisions offer the
    /// same. `None` when a surface cannot tell.
    #[must_use]
    pub fn revision(&self) -> Option<Vec<(usize, u64)>> {
        self.offered()
            .into_iter()
            .map(|(place, surface)| {
                surface
                    .borrow()
                    .revision()
                    .map(|revision| (place, revision))
            })
            .collect()
    }

    /// Everything the hosted surfaces offer now, in the order they were
    /// added.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        self.offered()
            .into_iter()
            .flat_map(|(_, surface)| surface.borrow_mut().resources())
            .collect()
    }

    /// Invoke the published action `address`, on the surface publishing
    /// it.
    ///
    /// # Errors
    /// No surface publishes it, or the surface refused.
    pub fn invoke(
        &self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        for (_, surface) in self.offered() {
            let mut surface = surface.borrow_mut();
            if ui::tilo_actions::door::resolve(&surface.resources(), address).is_some() {
                return surface.invoke(address, arguments);
            }
        }
        Err(DoorError::Unknown(address.clone()))
    }

    /// The labels of every hosted surface's accessibility tree.
    #[must_use]
    pub fn accessible_labels(&self) -> Vec<String> {
        self.offered()
            .into_iter()
            .flat_map(|(_, surface)| surface.borrow_mut().accessible_labels())
            .collect()
    }

    /// The reading of `resource` as the surface publishing it holds it now.
    #[must_use]
    pub fn read(&self, resource: &str) -> Option<serde_json::Value> {
        self.offered()
            .into_iter()
            .find_map(|(_, surface)| surface.borrow_mut().read(resource))
    }

    /// Where everything offered stands as one revision: equal revisions
    /// publish the same readings. `None` when a surface cannot tell.
    #[must_use]
    pub fn moved(&self) -> Option<u64> {
        Some(revision_of(&self.revision()?))
    }
}

/// One number for `revisions`, so two that match publish the same readings.
fn revision_of(revisions: &[(usize, u64)]) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    revisions.hash(&mut hasher);
    hasher.finish()
}
