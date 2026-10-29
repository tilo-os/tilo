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

use ui::host_operation::ResourceOperationStart;
use ui::tilo_actions::{ActionAddress, DoorError, ResourceSpec};

/// A hosted surface's published side.
pub trait Published {
    /// What it publishes now: its resources, with their actions.
    fn resources(&mut self) -> Vec<ResourceSpec>;

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

    /// Everything the hosted surfaces publish now, in the order they were
    /// added.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        self.surfaces
            .iter()
            .flat_map(|surface| surface.borrow_mut().resources())
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
        for surface in &self.surfaces {
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
        self.surfaces
            .iter()
            .flat_map(|surface| surface.borrow_mut().accessible_labels())
            .collect()
    }
}
